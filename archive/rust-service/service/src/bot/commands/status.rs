use async_trait::async_trait;
use sqlx::Row;

use crate::{
    app::AppState,
    bot::commands::{BotCommandHandler, CommandContext, CommandError},
    bot::domain::CommandDescriptor,
    platform::integration::BotRole,
};

pub struct StatusHandler {
    _state: AppState,
}

impl StatusHandler {
    pub fn new(state: AppState) -> Self {
        Self { _state: state }
    }
}

#[async_trait]
impl BotCommandHandler for StatusHandler {
    fn descriptor(&self) -> CommandDescriptor {
        CommandDescriptor {
            name: "status",
            aliases: &[],
            min_role: BotRole::ReadOnly,
            private_only: false,
            has_side_effects: false,
            usage: "/status",
            help: "查看账号、监控和通知摘要",
        }
    }

    async fn handle(
        &self,
        context: CommandContext,
        _arguments: &[String],
    ) -> Result<Vec<String>, CommandError> {
        let state = &context.state;

        let account = sqlx::query(
            "SELECT status, CAST(last_auth_checked_at AS TEXT) AS last_auth_checked_at
             FROM nga_accounts WHERE label = 'default'",
        )
        .fetch_optional(&state.pool)
        .await
        .map_err(|_| CommandError::internal())?;
        let account_line = match account {
            Some(row) => {
                let status: String = row.get("status");
                format!("NGA 账号：{}", status)
            }
            None => "NGA 账号：未配置".to_owned(),
        };

        let watch_counts = sqlx::query(
            "SELECT
               COUNT(*) AS total,
               COALESCE(SUM(CASE WHEN enabled = 1 THEN 1 ELSE 0 END), 0) AS enabled
             FROM watch_targets WHERE deleted_at IS NULL",
        )
        .fetch_one(&state.pool)
        .await
        .map_err(|_| CommandError::internal())?;
        let watch_total: i64 = watch_counts.get("total");
        let watch_enabled: i64 = watch_counts.get("enabled");

        let channels: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM notification_channels c
             JOIN platform_integrations i ON i.id = c.integration_id
             WHERE c.enabled = 1 AND i.enabled = 1 AND i.delivery_enabled = 1",
        )
        .fetch_one(&state.pool)
        .await
        .map_err(|_| CommandError::internal())?;

        let bots: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM platform_integrations
             WHERE bot_enabled = 1 AND enabled = 1",
        )
        .fetch_one(&state.pool)
        .await
        .map_err(|_| CommandError::internal())?;

        let role = context
            .binding
            .as_ref()
            .map(|binding| binding.role.as_str())
            .unwrap_or("unbound");

        Ok(vec![format!(
            "NGA Reminder 状态\n\
             {account_line}\n\
             监控目标：{watch_total} 个（启用 {watch_enabled} 个）\n\
             通知目标：{channels} 个启用\n\
             机器人连接：{bots} 个启用\n\
             你的角色：{role}"
        )])
    }
}
