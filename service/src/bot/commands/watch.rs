use async_trait::async_trait;
use sqlx::Row;

use crate::{
    app::AppState,
    bot::commands::{BotCommandHandler, CommandContext, CommandError, CommandErrorKind},
    bot::domain::CommandDescriptor,
    platform::integration::BotRole,
};

pub struct WatchHandler {
    _state: AppState,
}

impl WatchHandler {
    pub fn new(state: AppState) -> Self {
        Self { _state: state }
    }
}

#[async_trait]
impl BotCommandHandler for WatchHandler {
    fn descriptor(&self) -> CommandDescriptor {
        CommandDescriptor {
            name: "watch",
            aliases: &[],
            min_role: BotRole::ReadOnly,
            private_only: false,
            has_side_effects: true,
            usage: "/watch list | /watch run <watch_id>",
            help: "查看监控列表或触发一次立即运行",
        }
    }

    async fn handle(
        &self,
        context: CommandContext,
        arguments: &[String],
    ) -> Result<Vec<String>, CommandError> {
        let sub = arguments.first().map(String::as_str).unwrap_or("list");
        match sub {
            "list" => list(context).await,
            "run" => {
                let id = arguments.get(1).ok_or_else(|| {
                    CommandError::new(
                        CommandErrorKind::InvalidArguments,
                        "用法：/watch run <watch_id>",
                    )
                })?;
                run(context, id).await
            }
            _ => Err(CommandError::new(
                CommandErrorKind::InvalidArguments,
                "用法：/watch list | /watch run <watch_id>",
            )),
        }
    }
}

async fn list(context: CommandContext) -> Result<Vec<String>, CommandError> {
    let state = &context.state;
    let rows = sqlx::query(
        "SELECT id, target_type, target_id, enabled, status, pause_reason,
                CAST(next_run_at AS TEXT) AS next_run_at
         FROM watch_targets
         WHERE deleted_at IS NULL
         ORDER BY created_at
         LIMIT 30",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|_| CommandError::internal())?;
    if rows.is_empty() {
        return Ok(vec!["当前没有监控目标。请在管理台添加。".to_owned()]);
    }
    let mut lines = vec![format!("监控目标（{} 个）：", rows.len())];
    for row in rows {
        let id: String = row.get("id");
        let target_type: String = row.get("target_type");
        let target_id: i64 = row.get("target_id");
        let enabled: i32 = row.get("enabled");
        let status: String = row.get("status");
        let pause_reason: Option<String> = row.get("pause_reason");
        let pause = pause_reason
            .map(|reason| format!("（暂停：{reason}）"))
            .unwrap_or_default();
        let state_text = if enabled == 1 { "启用" } else { "已停用" };
        lines.push(format!(
            "`{id}` {target_type}:{target_id} {state_text} {status}{pause}"
        ));
    }
    Ok(vec![lines.join("\n")])
}

async fn run(context: CommandContext, id: &str) -> Result<Vec<String>, CommandError> {
    let state = &context.state;
    let claimed =
        crate::repository::watch::claim_by_id(&state.pool, state.config.database_backend, id)
            .await
            .map_err(|_| CommandError::internal())?;
    let Some(claimed) = claimed else {
        return Ok(vec!["监控目标不存在或正在运行中。".to_owned()]);
    };
    let result: Result<(), String> = match claimed.target_type.as_str() {
        "thread" => crate::collector::thread::run(state, claimed)
            .await
            .map(|_| ())
            .map_err(|error| format!("{error:?}")),
        "user" => crate::collector::user::run(state, claimed)
            .await
            .map(|_| ())
            .map_err(|error| format!("{error:?}")),
        _ => return Ok(vec!["不支持的监控类型。".to_owned()]),
    };
    match result {
        Ok(()) => Ok(vec!["运行完成。".to_owned()]),
        Err(_) => Ok(vec!["运行失败，请查看管理台的运行记录。".to_owned()]),
    }
}
