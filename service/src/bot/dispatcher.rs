//! Dispatches inbound events to command handlers with authorization,
//! idempotency and audit. Runs after the platform adapter normalizes events.

#![allow(dead_code)]

use crate::{
    app::AppState,
    bot::commands::{CommandContext, CommandRouter},
    bot::domain::BotEvent,
    bot::parser::{ParseError, parse},
    bot::repository::{self, InboundDedup},
    bot::{authorization, outbox},
};

pub enum DispatchOutcome {
    /// The event was not a slash command; nothing was enqueued.
    NotACommand,
    /// A command was handled (successfully or with a user-facing error).
    Handled,
    /// Duplicate platform message; no action taken.
    Duplicate,
}

/// Idempotency + audit wrapper around a single inbound event.
pub async fn dispatch(state: &AppState, event: &BotEvent) -> Result<DispatchOutcome, sqlx::Error> {
    let parsed = match parse(&event.text) {
        Ok(parsed) => parsed,
        Err(ParseError::NotACommand) => return Ok(DispatchOutcome::NotACommand),
        Err(_) => {
            // Malformed or overlong "commands" are ignored but still audited
            // as received-only rows; no reply is produced.
            let _ = repository::record_inbound_event(
                state,
                &event.integration_id,
                &event.platform_message_id,
                event.platform_event_id.as_deref(),
                &event.actor_id,
                &event.conversation_id,
                event.conversation_type,
                None,
                "received",
            )
            .await?;
            return Ok(DispatchOutcome::Handled);
        }
    };

    let router = CommandRouter::build_default(state.clone());
    let Some(handler) = router.find(&parsed.name) else {
        return reply_unknown_command(state, event).await;
    };

    // Register the event and dedupe against repeats of the same message.
    let (inbound_id, dedup) = repository::record_inbound_event(
        state,
        &event.integration_id,
        &event.platform_message_id,
        event.platform_event_id.as_deref(),
        &event.actor_id,
        &event.conversation_id,
        event.conversation_type,
        Some(&parsed.name),
        "processing",
    )
    .await?;
    if dedup == InboundDedup::Duplicate {
        crate::metrics::bot_inbound_duplicate();
        return Ok(DispatchOutcome::Duplicate);
    }
    if repository::is_rate_limited(state, &event.integration_id, &event.actor_id, &parsed.name)
        .await?
    {
        enqueue_reply(
            state,
            event,
            &inbound_id,
            &parsed.name,
            0,
            "命令请求过于频繁，请稍后再试。",
        )
        .await?;
        repository::mark_inbound_processed(state, &inbound_id, "rejected", Some("rate_limited"))
            .await?;
        crate::metrics::bot_inbound_rejected();
        return Ok(DispatchOutcome::Handled);
    }

    let binding = repository::find_binding(
        state,
        &event.integration_id,
        &event.actor_id,
        &event.conversation_id,
    )
    .await?;

    let descriptor = handler.descriptor();
    let rejection = if handler.allow_unbound() && binding.is_none() {
        // Unbound actors may only reach this handler from a private chat
        // (e.g. `/bind`); everything else requires a binding.
        if descriptor.private_only
            && event.conversation_type != crate::platform::integration::ConversationType::Private
        {
            authorization::Authorization::PrivateChatRequired
        } else {
            authorization::Authorization::Allowed
        }
    } else {
        authorization::authorize(
            event,
            binding.as_ref(),
            descriptor.min_role,
            descriptor.private_only,
        )
    };

    if rejection != authorization::Authorization::Allowed {
        let message = authorization::rejection_message(rejection);
        enqueue_reply(state, event, &inbound_id, &parsed.name, 0, &message).await?;
        repository::mark_inbound_processed(
            state,
            &inbound_id,
            "rejected",
            Some(rejection_kind(rejection)),
        )
        .await?;
        crate::metrics::bot_inbound_rejected();
        return Ok(DispatchOutcome::Handled);
    }

    let context = CommandContext {
        state: state.clone(),
        event: event.clone(),
        binding,
    };
    match handler.handle(context, &parsed.args).await {
        Ok(replies) => {
            for (sequence, reply) in replies.iter().enumerate() {
                enqueue_reply(state, event, &inbound_id, &parsed.name, sequence, reply).await?;
            }
            repository::mark_inbound_processed(state, &inbound_id, "succeeded", None).await?;
        }
        Err(error) => {
            enqueue_reply(state, event, &inbound_id, &parsed.name, 0, &error.message).await?;
            repository::mark_inbound_processed(
                state,
                &inbound_id,
                "failed",
                Some(error.kind.as_str()),
            )
            .await?;
        }
    }
    crate::metrics::bot_inbound_handled();
    Ok(DispatchOutcome::Handled)
}

async fn reply_unknown_command(
    state: &AppState,
    event: &BotEvent,
) -> Result<DispatchOutcome, sqlx::Error> {
    let (inbound_id, dedup) = repository::record_inbound_event(
        state,
        &event.integration_id,
        &event.platform_message_id,
        event.platform_event_id.as_deref(),
        &event.actor_id,
        &event.conversation_id,
        event.conversation_type,
        Some(&event_unknown_name(event)),
        "processing",
    )
    .await?;
    if dedup == InboundDedup::Duplicate {
        crate::metrics::bot_inbound_duplicate();
        return Ok(DispatchOutcome::Duplicate);
    }
    if repository::is_rate_limited(state, &event.integration_id, &event.actor_id, "unknown").await?
    {
        repository::mark_inbound_processed(state, &inbound_id, "rejected", Some("rate_limited"))
            .await?;
        crate::metrics::bot_inbound_rejected();
        return Ok(DispatchOutcome::Handled);
    }
    enqueue_reply(
        state,
        event,
        &inbound_id,
        "unknown",
        0,
        "未知命令，发送 /help 查看可用命令。",
    )
    .await?;
    repository::mark_inbound_processed(state, &inbound_id, "succeeded", None).await?;
    crate::metrics::bot_inbound_handled();
    Ok(DispatchOutcome::Handled)
}

fn event_unknown_name(event: &BotEvent) -> String {
    event
        .text
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_start_matches('/')
        .chars()
        .take(32)
        .collect()
}

fn rejection_kind(rejection: authorization::Authorization) -> &'static str {
    match rejection {
        authorization::Authorization::Unbound => "unbound",
        authorization::Authorization::InsufficientRole => "insufficient_role",
        authorization::Authorization::PrivateChatRequired => "private_chat_required",
        authorization::Authorization::Allowed => "allowed",
    }
}

async fn enqueue_reply(
    state: &AppState,
    event: &BotEvent,
    inbound_id: &str,
    command: &str,
    sequence: usize,
    text: &str,
) -> Result<(), sqlx::Error> {
    let dedupe_key = format!(
        "command:{}:{}:{}:{}",
        event.integration_id, event.platform_message_id, command, sequence
    );
    outbox::enqueue_text_reply(
        state,
        &event.integration_id,
        Some(inbound_id),
        &event.conversation_id,
        Some(&event.platform_message_id),
        &dedupe_key,
        text,
        None,
    )
    .await?;
    Ok(())
}
