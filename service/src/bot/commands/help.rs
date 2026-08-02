use async_trait::async_trait;

use crate::{
    app::AppState,
    bot::commands::{BotCommandHandler, CommandContext, CommandError, CommandRouter},
    bot::domain::CommandDescriptor,
    platform::integration::BotRole,
};

pub struct HelpHandler {
    _state: AppState,
}

impl HelpHandler {
    pub fn new(state: AppState) -> Self {
        Self { _state: state }
    }
}

#[async_trait]
impl BotCommandHandler for HelpHandler {
    fn descriptor(&self) -> CommandDescriptor {
        CommandDescriptor {
            name: "help",
            aliases: &[],
            min_role: BotRole::ReadOnly,
            private_only: false,
            has_side_effects: false,
            usage: "/help",
            help: "显示当前角色可用的命令",
        }
    }

    fn allow_unbound(&self) -> bool {
        true
    }

    async fn handle(
        &self,
        context: CommandContext,
        _arguments: &[String],
    ) -> Result<Vec<String>, CommandError> {
        let role = context
            .binding
            .as_ref()
            .map(|binding| binding.role)
            .unwrap_or(BotRole::ReadOnly);
        let router = CommandRouter::build_default(context.state);
        Ok(vec![router.help_text(role)])
    }
}
