use async_trait::async_trait;

use crate::{
    app::AppState,
    bot::commands::{BotCommandHandler, CommandContext, CommandError, CommandErrorKind},
    bot::domain::CommandDescriptor,
    platform::integration::BotRole,
};

pub struct BindHandler {
    _state: AppState,
}

impl BindHandler {
    pub fn new(state: AppState) -> Self {
        Self { _state: state }
    }
}

#[async_trait]
impl BotCommandHandler for BindHandler {
    fn descriptor(&self) -> CommandDescriptor {
        CommandDescriptor {
            name: "bind",
            aliases: &[],
            min_role: BotRole::ReadOnly,
            private_only: true,
            has_side_effects: true,
            usage: "/bind <code>",
            help: "使用管理台生成的一次性绑定码绑定身份",
        }
    }

    fn allow_unbound(&self) -> bool {
        true
    }

    async fn handle(
        &self,
        context: CommandContext,
        arguments: &[String],
    ) -> Result<Vec<String>, CommandError> {
        if context.binding.is_some() {
            return Ok(vec!["你已经绑定了身份，无需重复绑定。".to_owned()]);
        }
        let code = arguments.first().ok_or_else(|| {
            CommandError::new(CommandErrorKind::InvalidArguments, "用法：/bind <code>")
        })?;
        let code = code.trim();
        if code.is_empty() || !code.starts_with("bind-") {
            return Ok(vec![
                "绑定码无效或已过期。请到管理台重新生成绑定码。".to_owned(),
            ]);
        }

        let state = &context.state;
        let integration_id = &context.event.integration_id;
        let label = context
            .event
            .actor_display_name
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("已绑定用户");
        let Some((_binding_id, role)) =
            crate::platform::integration::consume_pairing_token_and_insert_binding(
                state,
                integration_id,
                code,
                &context.event.actor_id,
                &context.event.conversation_id,
                label,
            )
            .await
            .map_err(|_| CommandError::internal())?
        else {
            return Ok(vec![
                "绑定码无效或已过期。请到管理台重新生成绑定码。".to_owned(),
            ]);
        };

        Ok(vec![format!(
            "绑定成功！你的角色是 {}。发送 /help 查看可用命令。",
            role.as_str()
        )])
    }
}
