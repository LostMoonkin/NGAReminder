use async_trait::async_trait;
use sqlx::Row;

use crate::{
    app::AppState,
    bot::authorization::has_minimum_role,
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
        let required_role = subcommand_min_role(sub);
        let role = context
            .binding
            .as_ref()
            .map(|binding| binding.role)
            .ok_or_else(CommandError::internal)?;
        if !has_minimum_role(role, required_role) {
            return Err(CommandError::new(
                CommandErrorKind::Conflict,
                "当前命令对你不可用。",
            ));
        }
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

fn subcommand_min_role(subcommand: &str) -> BotRole {
    if subcommand == "run" {
        BotRole::Operator
    } else {
        BotRole::ReadOnly
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
    let requested = crate::repository::watch::request_run(&state.pool, id)
        .await
        .map_err(|_| CommandError::internal())?;
    if requested {
        Ok(vec![
            "已安排立即运行，可稍后通过 /watch list 查看状态。".to_owned(),
        ])
    } else {
        Ok(vec!["监控目标不存在、未启用或正在运行中。".to_owned()])
    }
}

#[cfg(test)]
mod tests {
    use super::subcommand_min_role;
    use crate::platform::integration::BotRole;

    #[test]
    fn run_requires_operator_while_list_is_read_only() {
        assert_eq!(subcommand_min_role("list"), BotRole::ReadOnly);
        assert_eq!(subcommand_min_role("run"), BotRole::Operator);
    }
}
