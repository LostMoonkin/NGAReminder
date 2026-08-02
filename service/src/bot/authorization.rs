//! Authorization rules for bot commands. The dispatcher runs these checks
//! before any handler executes; handlers never re-authorize.

use crate::{
    bot::domain::BotEvent,
    bot::repository::BotBindingInfo,
    platform::integration::{BotRole, ConversationType},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Authorization {
    Allowed,
    Unbound,
    InsufficientRole,
    PrivateChatRequired,
}

/// Decide whether `binding` (if any) may run a command with the given role
/// requirement. Login commands additionally require owner + private chat,
/// which the caller enforces through `private_only` and `min_role`.
pub fn authorize(
    event: &BotEvent,
    binding: Option<&BotBindingInfo>,
    min_role: BotRole,
    private_only: bool,
) -> Authorization {
    if private_only && event.conversation_type != ConversationType::Private {
        return Authorization::PrivateChatRequired;
    }
    let Some(binding) = binding else {
        return Authorization::Unbound;
    };
    if role_rank(binding.role) < role_rank(min_role) {
        return Authorization::InsufficientRole;
    }
    Authorization::Allowed
}

fn role_rank(role: BotRole) -> u8 {
    match role {
        BotRole::Owner => 3,
        BotRole::Operator => 2,
        BotRole::ReadOnly => 1,
    }
}

pub fn has_minimum_role(actual: BotRole, required: BotRole) -> bool {
    role_rank(actual) >= role_rank(required)
}

/// Stable user-facing replies for authorization failures.
pub fn rejection_message(rejection: Authorization) -> String {
    match rejection {
        Authorization::Unbound => {
            "你还没有绑定身份。请在管理台「机器人授权」中生成一次性绑定码，然后私聊机器人发送 /bind <code>。"
                .to_owned()
        }
        Authorization::InsufficientRole => {
            "当前命令对你不可用。".to_owned()
        }
        Authorization::PrivateChatRequired => {
            "该命令仅支持私聊机器人使用。".to_owned()
        }
        Authorization::Allowed => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;

    use super::authorize;
    use crate::{
        bot::domain::{BotEvent, BotPlatform},
        bot::repository::BotBindingInfo,
        platform::integration::{BotRole, ConversationType},
    };

    fn event(conversation_type: ConversationType) -> BotEvent {
        BotEvent {
            integration_id: "i".to_owned(),
            platform: BotPlatform::Feishu,
            platform_event_id: None,
            platform_message_id: "m".to_owned(),
            actor_id: "actor".to_owned(),
            actor_display_name: None,
            conversation_id: "c".to_owned(),
            conversation_type,
            text: "/status".to_owned(),
            mentions: vec![],
            occurred_at: OffsetDateTime::now_utc(),
        }
    }

    fn binding(role: BotRole) -> BotBindingInfo {
        BotBindingInfo {
            id: "b".to_owned(),
            integration_id: "i".to_owned(),
            actor_id: "actor".to_owned(),
            role,
            label: "test".to_owned(),
        }
    }

    #[test]
    fn owner_passes_owner_private_only() {
        assert_eq!(
            authorize(
                &event(ConversationType::Private),
                Some(&binding(BotRole::Owner)),
                BotRole::Owner,
                true,
            ),
            super::Authorization::Allowed
        );
    }

    #[test]
    fn group_login_is_rejected_before_role_check() {
        assert_eq!(
            authorize(
                &event(ConversationType::Group),
                Some(&binding(BotRole::Owner)),
                BotRole::Owner,
                true,
            ),
            super::Authorization::PrivateChatRequired
        );
    }

    #[test]
    fn read_only_cannot_run_operator_command() {
        assert_eq!(
            authorize(
                &event(ConversationType::Private),
                Some(&binding(BotRole::ReadOnly)),
                BotRole::Operator,
                false,
            ),
            super::Authorization::InsufficientRole
        );
    }

    #[test]
    fn unbound_actor_is_rejected() {
        assert_eq!(
            authorize(
                &event(ConversationType::Private),
                None,
                BotRole::ReadOnly,
                false,
            ),
            super::Authorization::Unbound
        );
    }
}
