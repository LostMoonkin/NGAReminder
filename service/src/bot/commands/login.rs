use async_trait::async_trait;
use secrecy::ExposeSecret;
use tracing::warn;

use crate::{
    app::AppState,
    bot::commands::{BotCommandHandler, CommandContext, CommandError, CommandErrorKind},
    bot::domain::{CommandDescriptor, ImagePayload},
    bot::session::{self, LoginSession, LoginSessionStatus, MAX_CAPTCHA_ATTEMPTS},
    nga::{AuthCheckError, login::NgaWebLoginV1},
    platform::integration::BotRole,
};

pub struct LoginHandler {
    _state: AppState,
}

impl LoginHandler {
    pub fn new(state: AppState) -> Self {
        Self { _state: state }
    }
}

#[async_trait]
impl BotCommandHandler for LoginHandler {
    fn descriptor(&self) -> CommandDescriptor {
        CommandDescriptor {
            name: "login",
            aliases: &[],
            min_role: BotRole::Owner,
            private_only: true,
            has_side_effects: true,
            usage: "/login status | /login confirm <request_id> | /login captcha <request_id> <code> | /login cancel <request_id>",
            help: "查看或推进 NGA Cookie 自动续期",
        }
    }

    async fn handle(
        &self,
        context: CommandContext,
        arguments: &[String],
    ) -> Result<Vec<String>, CommandError> {
        let sub = arguments.first().map(String::as_str).unwrap_or("status");
        match sub {
            "status" => status(context).await,
            "confirm" => {
                let request_id = arguments.get(1).ok_or_else(|| {
                    CommandError::new(
                        CommandErrorKind::InvalidArguments,
                        "用法：/login confirm <request_id>",
                    )
                })?;
                confirm(context, request_id).await
            }
            "captcha" => {
                let request_id = arguments.get(1).ok_or_else(|| {
                    CommandError::new(
                        CommandErrorKind::InvalidArguments,
                        "用法：/login captcha <request_id> <code>",
                    )
                })?;
                let code = arguments.get(2).ok_or_else(|| {
                    CommandError::new(
                        CommandErrorKind::InvalidArguments,
                        "用法：/login captcha <request_id> <code>",
                    )
                })?;
                captcha(context, request_id, code).await
            }
            "cancel" => {
                let request_id = arguments.get(1).ok_or_else(|| {
                    CommandError::new(
                        CommandErrorKind::InvalidArguments,
                        "用法：/login cancel <request_id>",
                    )
                })?;
                cancel(context, request_id).await
            }
            _ => Err(CommandError::new(
                CommandErrorKind::InvalidArguments,
                "用法：/login status | /login confirm <request_id> | /login captcha <request_id> <code> | /login cancel <request_id>",
            )),
        }
    }
}

/// The session must belong to this actor/integration/private conversation.
fn owns_session(event: &crate::bot::domain::BotEvent, session: &LoginSession) -> bool {
    session.actor_id == event.actor_id
        && session.integration_id == event.integration_id
        && session.conversation_id == event.conversation_id
}

async fn status(context: CommandContext) -> Result<Vec<String>, CommandError> {
    let state = &context.state;
    let account_id = default_account_id(state).await?;
    let Some(account_id) = account_id else {
        return Ok(vec!["NGA 账号未配置。".to_owned()]);
    };
    let active = session::active_session_for_account(state, &account_id)
        .await
        .map_err(|_| CommandError::internal())?;
    let setting = session::renewal_setting_view(state, &account_id)
        .await
        .map_err(|_| CommandError::internal())?;

    let mut lines = vec!["NGA Cookie 续期状态：".to_owned()];
    match setting {
        Some(setting) => {
            lines.push(format!(
                "续期配置：{}，凭据状态：{}",
                if setting.enabled {
                    "已启用"
                } else {
                    "已停用"
                },
                setting.credential_status
            ));
            if let Some(cooldown) = setting.cooldown_until {
                lines.push(format!("冷却截止：{cooldown}"));
            }
            if let Some(error) = setting.last_error_kind {
                lines.push(format!("最近错误：{error}"));
            }
        }
        None => lines.push("尚未配置续期凭据。".to_owned()),
    }
    match active {
        Some(session) => {
            lines.push(format!(
                "活动会话：{}（{}）",
                session.id,
                session.status.as_str()
            ));
        }
        None => lines.push("当前没有进行中的续期会话。".to_owned()),
    }
    Ok(vec![lines.join("\n")])
}

async fn confirm(context: CommandContext, request_id: &str) -> Result<Vec<String>, CommandError> {
    let state = &context.state;
    let Some(session) = session::get_session(state, request_id)
        .await
        .map_err(|_| CommandError::internal())?
    else {
        return Ok(vec!["续期请求不存在或已结束。".to_owned()]);
    };
    if !owns_session(&context.event, &session) {
        return Ok(vec!["该续期请求不属于你，无法操作。".to_owned()]);
    }
    if session.status != LoginSessionStatus::AwaitingConfirmation {
        return Ok(vec![format!(
            "该续期请求当前状态为 {}，无法确认。",
            session.status.as_str()
        )]);
    }
    let claimed = session::transition(
        state,
        &session.id,
        &[LoginSessionStatus::AwaitingConfirmation],
        LoginSessionStatus::Starting,
        None,
    )
    .await
    .map_err(|_| CommandError::internal())?;
    if !claimed {
        return Ok(vec![
            "续期请求已被其他操作处理，请发送 /login status 查看。".to_owned(),
        ]);
    }

    // Challenge preparation does not need the password. Renewal credentials
    // remain encrypted until the user submits the captcha.
    let mut adapter = NgaWebLoginV1::new(state.config.nga_user_agent.as_str())
        .map_err(|_| CommandError::internal())?;
    let challenge = match adapter.prepare_challenge().await {
        Ok(challenge) => challenge,
        Err(error) => {
            warn!(
                session_id = %session.id,
                account_id = %session.account_id,
                phase = "prepare_challenge",
                error_kind = error.kind(),
                error = %error,
                "NGA Cookie renewal failed"
            );
            fail_with_kind(state, &session, error.kind()).await?;
            return Ok(vec![
                "验证码获取失败，续期已终止。请稍后重试或到管理台手动更新 Cookie。".to_owned(),
            ]);
        }
    };

    let moved = session::store_challenge_and_enqueue(
        state,
        &session,
        LoginSessionStatus::Starting,
        &challenge.context,
        ImagePayload {
            mime_type: challenge.image_mime,
            bytes: challenge.image,
        },
        challenge.expires_at,
    )
    .await
    .map_err(|_| CommandError::internal())?;
    if !moved {
        return Ok(vec!["续期请求状态已变化，请重新确认。".to_owned()]);
    }

    // The worker sends the instruction only after Feishu confirms that the
    // image was uploaded and delivered. Returning success text here would
    // falsely claim delivery when the image outbox later fails.
    Ok(Vec::new())
}

async fn captcha(
    context: CommandContext,
    request_id: &str,
    code: &str,
) -> Result<Vec<String>, CommandError> {
    let state = &context.state;
    let code = code.trim();
    if !is_valid_captcha(code) {
        return Ok(vec!["验证码应为 6 位字母或数字。".to_owned()]);
    }
    let Some(session) = session::get_session(state, request_id)
        .await
        .map_err(|_| CommandError::internal())?
    else {
        return Ok(vec!["续期请求不存在或已结束。".to_owned()]);
    };
    if !owns_session(&context.event, &session) {
        return Ok(vec!["该续期请求不属于你，无法操作。".to_owned()]);
    }
    if session.status != LoginSessionStatus::AwaitingCaptcha {
        return Ok(vec![format!(
            "该续期请求当前状态为 {}，无法提交验证码。",
            session.status.as_str()
        )]);
    }
    let claimed = session::transition(
        state,
        &session.id,
        &[LoginSessionStatus::AwaitingCaptcha],
        LoginSessionStatus::Submitting,
        None,
    )
    .await
    .map_err(|_| CommandError::internal())?;
    if !claimed {
        return Ok(vec![
            "验证码已被提交过，请发送 /login status 查看。".to_owned(),
        ]);
    }

    let Some(context_data) = session::load_protocol_context(state, &session.id)
        .await
        .map_err(|_| CommandError::internal())?
    else {
        session::transition(
            state,
            &session.id,
            &[LoginSessionStatus::Submitting],
            LoginSessionStatus::Failed,
            Some("candidate_cookie_missing"),
        )
        .await
        .map_err(|_| CommandError::internal())?;
        return Ok(vec!["登录会话已失效，请重新发起续期。".to_owned()]);
    };
    if !context_data.is_current_and_unexpired() {
        fail_with_kind(state, &session, "captcha_expired").await?;
        return Ok(vec!["登录会话或验证码已过期，请重新发起续期。".to_owned()]);
    }
    let Some(credentials) = session::load_renewal_credentials(state, &session.account_id)
        .await
        .map_err(|_| CommandError::internal())?
    else {
        fail_with_kind(state, &session, "renewal_not_configured").await?;
        return Ok(vec!["续期凭据未配置。".to_owned()]);
    };

    let mut adapter = NgaWebLoginV1::new(state.config.nga_user_agent.as_str())
        .map_err(|_| CommandError::internal())?;
    let step = adapter
        .submit_login(
            &credentials.login_name,
            &credentials.password,
            &context_data,
            code,
        )
        .await;

    match step {
        Ok(crate::nga::login::LoginStep::CookieCandidate {
            passport_uid,
            passport_cid,
            cookie_header,
        }) => {
            let uid = passport_uid.expose_secret().to_string();
            let cid = passport_cid.expose_secret().to_string();
            let cookie = cookie_header.expose_secret().to_string();
            let moved = session::transition(
                state,
                &session.id,
                &[LoginSessionStatus::Submitting],
                LoginSessionStatus::ValidatingCookie,
                None,
            )
            .await
            .map_err(|_| CommandError::internal())?;
            if !moved {
                return Ok(vec!["续期请求状态已变化。".to_owned()]);
            }
            match state.nga_client.check_credentials(&uid, &cookie).await {
                Ok(check) if check.uid.to_string() == uid => {
                    let restored = session::complete_success(
                        state,
                        &session.id,
                        &session.account_id,
                        &uid,
                        &cid,
                        &cookie,
                    )
                    .await
                    .map_err(|_| CommandError::internal())?;
                    match restored {
                        Some(restored) => Ok(vec![format!(
                            "✅ Cookie 续期成功！已恢复 {restored} 个因认证暂停的监控。"
                        )]),
                        None => Ok(vec!["续期请求状态已变化。".to_owned()]),
                    }
                }
                Ok(_) => {
                    warn!(
                        session_id = %session.id,
                        account_id = %session.account_id,
                        phase = "validate_cookie",
                        error_kind = "candidate_uid_mismatch",
                        "NGA Cookie renewal failed"
                    );
                    session::mark_renewal_failure(
                        state,
                        &session.account_id,
                        "candidate_uid_mismatch",
                        false,
                    )
                    .await
                    .map_err(|_| CommandError::internal())?;
                    fail_with_kind(state, &session, "candidate_uid_mismatch").await?;
                    Ok(vec![
                        "新 Cookie 验证未通过（UID 不一致），已保留旧 Cookie。请到管理台手动更新。"
                            .to_owned(),
                    ])
                }
                Err(AuthCheckError::Unauthorized) => {
                    warn!(
                        session_id = %session.id,
                        account_id = %session.account_id,
                        phase = "validate_cookie",
                        error_kind = "candidate_cookie_invalid",
                        "NGA Cookie renewal failed"
                    );
                    session::mark_renewal_failure(
                        state,
                        &session.account_id,
                        "candidate_cookie_invalid",
                        false,
                    )
                    .await
                    .map_err(|_| CommandError::internal())?;
                    fail_with_kind(state, &session, "candidate_cookie_invalid").await?;
                    Ok(vec![
                        "新 Cookie 验证未通过，已保留旧 Cookie。请到管理台手动更新。".to_owned(),
                    ])
                }
                Err(error) => {
                    warn!(
                        session_id = %session.id,
                        account_id = %session.account_id,
                        phase = "validate_cookie",
                        error_kind = "candidate_cookie_check_failed",
                        error = %error,
                        "NGA Cookie renewal failed"
                    );
                    session::mark_renewal_failure(
                        state,
                        &session.account_id,
                        "candidate_cookie_invalid",
                        false,
                    )
                    .await
                    .map_err(|_| CommandError::internal())?;
                    fail_with_kind(state, &session, "candidate_cookie_invalid").await?;
                    Ok(vec![
                        "新 Cookie 验证失败，已保留旧 Cookie。请稍后重试或到管理台手动更新。"
                            .to_owned(),
                    ])
                }
            }
        }
        Ok(crate::nga::login::LoginStep::UnsupportedChallenge { kind }) => {
            let error_kind = if kind == "tencent" {
                "unsupported_tencent_captcha"
            } else {
                "unsupported_phone_verification"
            };
            session::transition(
                state,
                &session.id,
                &[LoginSessionStatus::Submitting],
                LoginSessionStatus::UnsupportedChallenge,
                Some(error_kind),
            )
            .await
            .map_err(|_| CommandError::internal())?;
            session::clear_protocol_context(state, &session.id)
                .await
                .map_err(|_| CommandError::internal())?;
            Ok(vec![
                "登录遇到额外的安全验证，暂不支持自动处理。请到管理台手动更新 Cookie。".to_owned(),
            ])
        }
        Err(error) => {
            warn!(
                session_id = %session.id,
                account_id = %session.account_id,
                phase = "submit_login",
                error_kind = error.kind(),
                error = %error,
                "NGA Cookie renewal failed"
            );
            match error.kind() {
                "captcha_invalid" | "captcha_expired" => {
                    let attempts = session.captcha_attempt_count + 1;
                    session::increment_captcha_attempts(state, &session.id)
                        .await
                        .map_err(|_| CommandError::internal())?;
                    if attempts >= MAX_CAPTCHA_ATTEMPTS {
                        session::mark_renewal_failure(
                            state,
                            &session.account_id,
                            "captcha_invalid",
                            false,
                        )
                        .await
                        .map_err(|_| CommandError::internal())?;
                        fail_with_kind(state, &session, "captcha_invalid").await?;
                        return Ok(vec!["验证码错误次数过多，续期已暂停 15 分钟。".to_owned()]);
                    }
                    // Fresh challenge with a new rid/prid and revision.
                    let mut adapter = NgaWebLoginV1::new(state.config.nga_user_agent.as_str())
                        .map_err(|_| CommandError::internal())?;
                    match adapter.prepare_challenge().await {
                        Ok(challenge) => {
                            let mut refreshed = challenge.context.clone();
                            refreshed.captcha_revision = context_data.captcha_revision + 1;
                            let moved = session::store_challenge_and_enqueue(
                                state,
                                &session,
                                LoginSessionStatus::Submitting,
                                &refreshed,
                                ImagePayload {
                                    mime_type: challenge.image_mime,
                                    bytes: challenge.image,
                                },
                                challenge.expires_at,
                            )
                            .await
                            .map_err(|_| CommandError::internal())?;
                            if !moved {
                                return Ok(vec!["续期请求状态已变化。".to_owned()]);
                            }
                            // As with the first challenge, the worker sends
                            // instructions only after confirmed image delivery.
                            Ok(Vec::new())
                        }
                        Err(prepare_error) => {
                            warn!(
                                session_id = %session.id,
                                account_id = %session.account_id,
                                phase = "refresh_challenge",
                                error_kind = prepare_error.kind(),
                                error = %prepare_error,
                                "NGA Cookie renewal failed"
                            );
                            fail_with_kind(state, &session, prepare_error.kind()).await?;
                            Ok(vec!["新验证码获取失败，续期已终止。请稍后重试或到管理台手动更新 Cookie。".to_owned()])
                        }
                    }
                }
                "invalid_renewal_credentials" => {
                    session::mark_renewal_failure(state, &session.account_id, error.kind(), true)
                        .await
                        .map_err(|_| CommandError::internal())?;
                    fail_with_kind(state, &session, error.kind()).await?;
                    Ok(vec![
                        "登录名或密码错误，自动续期已停用。请到管理台更新凭据。".to_owned(),
                    ])
                }
                "nga_login_busy" | "nga_login_http_error" => {
                    session::mark_renewal_failure(state, &session.account_id, error.kind(), false)
                        .await
                        .map_err(|_| CommandError::internal())?;
                    fail_with_kind(state, &session, error.kind()).await?;
                    Ok(vec![
                        "NGA 暂时繁忙，续期已暂停。请稍后重新发起。".to_owned(),
                    ])
                }
                "nga_login_protocol_changed" => {
                    session::mark_renewal_failure(state, &session.account_id, error.kind(), false)
                        .await
                        .map_err(|_| CommandError::internal())?;
                    fail_with_kind(state, &session, error.kind()).await?;
                    Ok(vec![
                        "NGA 登录协议发生变化，自动续期已停止。请到管理台手动更新 Cookie。"
                            .to_owned(),
                    ])
                }
                "candidate_cookie_missing" => {
                    session::mark_renewal_failure(state, &session.account_id, error.kind(), false)
                        .await
                        .map_err(|_| CommandError::internal())?;
                    fail_with_kind(state, &session, error.kind()).await?;
                    Ok(vec![
                        "NGA 登录响应中没有找到可识别的新 Cookie，已保留旧 Cookie。错误类型：candidate_cookie_missing；请查看服务端结构化日志后重新发起续期。"
                            .to_owned(),
                    ])
                }
                other => {
                    fail_with_kind(state, &session, other).await?;
                    Ok(vec!["续期失败，请到管理台手动更新 Cookie。".to_owned()])
                }
            }
        }
    }
}

async fn cancel(context: CommandContext, request_id: &str) -> Result<Vec<String>, CommandError> {
    let state = &context.state;
    let Some(session) = session::get_session(state, request_id)
        .await
        .map_err(|_| CommandError::internal())?
    else {
        return Ok(vec!["续期请求不存在或已结束。".to_owned()]);
    };
    if !owns_session(&context.event, &session) {
        return Ok(vec!["该续期请求不属于你，无法操作。".to_owned()]);
    }
    let cancelled = session::transition(
        state,
        &session.id,
        &[
            LoginSessionStatus::AwaitingConfirmation,
            LoginSessionStatus::Starting,
            LoginSessionStatus::AwaitingCaptcha,
            LoginSessionStatus::Submitting,
            LoginSessionStatus::ValidatingCookie,
        ],
        LoginSessionStatus::Cancelled,
        None,
    )
    .await
    .map_err(|_| CommandError::internal())?;
    if !cancelled {
        return Ok(vec!["该续期请求已经结束。".to_owned()]);
    }
    session::clear_protocol_context(state, &session.id)
        .await
        .map_err(|_| CommandError::internal())?;
    Ok(vec![
        "已取消续期。监控保持暂停，请到管理台手动更新 Cookie 后恢复。".to_owned(),
    ])
}

fn is_valid_captcha(code: &str) -> bool {
    code.len() == 6 && code.chars().all(|ch| ch.is_ascii_alphanumeric())
}

async fn default_account_id(state: &AppState) -> Result<Option<String>, CommandError> {
    let account_id: Option<String> =
        sqlx::query_scalar("SELECT id FROM nga_accounts WHERE label = 'default'")
            .fetch_optional(&state.pool)
            .await
            .map_err(|_| CommandError::internal())?;
    Ok(account_id)
}

async fn fail_with_kind(
    state: &AppState,
    session: &LoginSession,
    kind: &str,
) -> Result<(), CommandError> {
    session::transition(
        state,
        &session.id,
        &[
            LoginSessionStatus::Starting,
            LoginSessionStatus::Submitting,
            LoginSessionStatus::ValidatingCookie,
            LoginSessionStatus::AwaitingCaptcha,
        ],
        LoginSessionStatus::Failed,
        Some(kind),
    )
    .await
    .map_err(|_| CommandError::internal())?;
    session::clear_protocol_context(state, &session.id)
        .await
        .map_err(|_| CommandError::internal())?;
    Ok(())
}
