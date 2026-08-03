//! `/command` parsing. Only strict slash commands are supported in this phase;
//! no natural-language intent detection.

/// A parsed command: normalized lowercase name plus raw arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedCommand {
    pub name: String,
    pub args: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    NotACommand,
    EmptyName,
    InvalidName,
    TooLong,
}

pub const MAX_COMMAND_LENGTH: usize = 512;

/// Strip a platform-generated mention of the bot itself (e.g. Feishu group
/// messages prefix `@机器人`) before parsing. `mentions` carries the raw
/// mention list; any leading text that exactly matches a self mention is
/// removed together with surrounding whitespace.
pub fn strip_self_mention(text: &str, self_mention: Option<&str>) -> String {
    let Some(mention) = self_mention else {
        return text.to_owned();
    };
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix(mention) {
        return rest.trim_start().to_owned();
    }
    text.to_owned()
}

/// Parse a slash command from (possibly mention-stripped) text.
pub fn parse(text: &str) -> Result<ParsedCommand, ParseError> {
    if text.chars().count() > MAX_COMMAND_LENGTH {
        return Err(ParseError::TooLong);
    }
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix('/') else {
        return Err(ParseError::NotACommand);
    };
    let mut parts = rest.split_whitespace();
    let raw_name = parts.next().unwrap_or_default();
    let name = raw_name.to_ascii_lowercase();
    if name.is_empty() {
        return Err(ParseError::EmptyName);
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
    {
        return Err(ParseError::InvalidName);
    }
    Ok(ParsedCommand {
        name,
        args: parts.map(str::to_owned).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::{MAX_COMMAND_LENGTH, ParseError, parse, strip_self_mention};

    #[test]
    fn parses_simple_command() {
        let command = parse("/status").expect("must parse");
        assert_eq!(command.name, "status");
        assert!(command.args.is_empty());
    }

    #[test]
    fn command_name_is_case_insensitive_and_arguments_preserved() {
        let command = parse("/Watch Run  abc_123 中文").expect("must parse");
        assert_eq!(command.name, "watch");
        // Command names are lowercased; arguments keep their original case.
        assert_eq!(command.args, ["Run", "abc_123", "中文"]);
    }

    #[test]
    fn rejects_invalid_names() {
        assert_eq!(parse("/斜杠").unwrap_err(), ParseError::InvalidName);
        assert_eq!(parse("//double").unwrap_err(), ParseError::InvalidName);
        assert_eq!(parse("/bad!").unwrap_err(), ParseError::InvalidName);
    }

    #[test]
    fn rejects_plain_text_and_empty() {
        assert_eq!(parse("hello"), Err(ParseError::NotACommand));
        assert_eq!(parse("/"), Err(ParseError::EmptyName));
    }

    #[test]
    fn rejects_overlong_messages() {
        let long = format!("/{}", "x".repeat(MAX_COMMAND_LENGTH + 1));
        assert_eq!(parse(&long), Err(ParseError::TooLong));
    }

    #[test]
    fn strips_leading_self_mention() {
        assert_eq!(strip_self_mention("@bot /status", Some("@bot")), "/status");
        assert_eq!(
            strip_self_mention("  @bot  /status x", Some("@bot")),
            "/status x"
        );
        // Non-leading mention is preserved for the parser to reject.
        assert_eq!(
            strip_self_mention("hello @bot /status", Some("@bot")),
            "hello @bot /status"
        );
    }
}
