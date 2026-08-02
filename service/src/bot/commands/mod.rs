//! Command handlers and the command router. Handlers receive a normalized
//! `CommandContext` and return text replies; the dispatcher handles
//! authorization, idempotency and outbox enqueueing.

#![allow(dead_code)]
pub mod bind;
pub mod help;
pub mod login;
pub mod status;
pub mod watch;

use async_trait::async_trait;
use std::sync::Arc;

use crate::{
    app::AppState,
    bot::domain::{BotEvent, CommandDescriptor},
    bot::repository::BotBindingInfo,
    platform::integration::BotRole,
};

/// Everything a handler needs. `binding` is `None` only for unbound actors
/// running `/bind`.
#[derive(Clone)]
pub struct CommandContext {
    pub state: AppState,
    pub event: BotEvent,
    pub binding: Option<BotBindingInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandErrorKind {
    InvalidArguments,
    NotFound,
    Conflict,
    Busy,
    Internal,
}

impl CommandErrorKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::InvalidArguments => "invalid_arguments",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::Busy => "busy",
            Self::Internal => "internal",
        }
    }
}

/// A user-facing error. `message` is stable text; `kind` goes into the
/// inbound-event audit and logs.
#[derive(Clone, Debug)]
pub struct CommandError {
    pub kind: CommandErrorKind,
    pub message: String,
}

impl CommandError {
    pub fn new(kind: CommandErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn internal() -> Self {
        Self::new(
            CommandErrorKind::Internal,
            "系统错误，请稍后重试。".to_owned(),
        )
    }
}

#[async_trait]
pub trait BotCommandHandler: Send + Sync {
    fn descriptor(&self) -> CommandDescriptor;

    /// Bind is reachable for unbound actors; everything else requires a
    /// binding.
    fn allow_unbound(&self) -> bool {
        false
    }

    async fn handle(
        &self,
        context: CommandContext,
        arguments: &[String],
    ) -> Result<Vec<String>, CommandError>;
}

pub struct CommandRouter {
    handlers: Vec<Arc<dyn BotCommandHandler>>,
}

impl CommandRouter {
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
        }
    }

    pub fn register(&mut self, handler: Arc<dyn BotCommandHandler>) {
        self.handlers.push(handler);
    }

    pub fn build_default(state: AppState) -> Self {
        let mut router = Self::new();
        router.register(Arc::new(help::HelpHandler::new(state.clone())));
        router.register(Arc::new(status::StatusHandler::new(state.clone())));
        router.register(Arc::new(bind::BindHandler::new(state.clone())));
        router.register(Arc::new(watch::WatchHandler::new(state.clone())));
        router.register(Arc::new(login::LoginHandler::new(state)));
        router
    }

    /// Find a handler by command name or alias.
    pub fn find(&self, name: &str) -> Option<Arc<dyn BotCommandHandler>> {
        self.handlers
            .iter()
            .find(|handler| {
                let descriptor = handler.descriptor();
                descriptor.name == name || descriptor.aliases.contains(&name)
            })
            .cloned()
    }

    /// Help text listing commands visible to a role.
    pub fn help_text(&self, role: BotRole) -> String {
        let mut lines = vec!["可用命令：".to_owned()];
        let mut descriptors: Vec<CommandDescriptor> = self
            .handlers
            .iter()
            .map(|handler| handler.descriptor())
            .collect();
        descriptors.sort_by_key(|descriptor| descriptor.name);
        for descriptor in descriptors {
            if crate::bot::authorization::has_minimum_role(role, descriptor.min_role) {
                let scope = if descriptor.private_only {
                    "（私聊）"
                } else {
                    ""
                };
                lines.push(format!(
                    "`{}`{scope} — {}",
                    descriptor.usage, descriptor.help
                ));
            }
        }
        lines.push("更多能力请在管理台查看。".to_owned());
        lines.join("\n")
    }
}
