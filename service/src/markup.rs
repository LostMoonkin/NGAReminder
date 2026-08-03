use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkupNode {
    Text(String),
    Bold(Vec<MarkupNode>),
    Italic(Vec<MarkupNode>),
    Underline(Vec<MarkupNode>),
    Strike(Vec<MarkupNode>),
    Quote(Vec<MarkupNode>),
    Link {
        url: String,
        children: Vec<MarkupNode>,
    },
    Image(String),
    Code(String),
    LineBreak,
    HorizontalRule,
}

const MAX_DEPTH: usize = 64;

pub fn parse(input: &str) -> Vec<MarkupNode> {
    let normalized = normalize_source_markup(input);
    parse_normalized(&normalized)
}

pub fn render_markdown(input: &str, assets: &HashMap<String, String>) -> String {
    let normalized = normalize_source_markup(input);
    if let Some((quoted, body)) = split_nga_reply_quote(&normalized) {
        let quoted = render_nodes_to_string(&parse_normalized(quoted), assets);
        let body = render_nodes_to_string(&parse_normalized(body), assets);
        let mut output = render_blockquote(&quoted);
        output.push('\n');
        output.push_str(&body);
        return normalise_output(&output);
    }
    let mut output = String::new();
    render_nodes(&parse_normalized(&normalized), assets, &mut output);
    normalise_output(&output)
}

/// Render NGA markup as a small, safe HTML subset for the embedded admin UI.
///
/// Text and attributes are escaped, and links/images are emitted only for URLs
/// already accepted by the parser's HTTP(S)-only policy.
pub fn render_html(input: &str) -> String {
    let normalized = normalize_source_markup(input);
    if let Some((quoted, body)) = split_nga_reply_quote(&normalized) {
        let mut output = String::from("<blockquote>");
        render_html_nodes(&parse_normalized(quoted), &mut output);
        output.push_str("</blockquote>");
        render_html_nodes(&parse_normalized(body), &mut output);
        return output;
    }
    let mut output = String::new();
    render_html_nodes(&parse_normalized(&normalized), &mut output);
    output
}

/// Render post content for compact notification cards.
///
/// This starts from the same renderer as Markdown exports, then removes code
/// fences that are useful in a document but too noisy in a push notification.
/// Non-empty code content is retained as ordinary text.
pub fn render_compact_markdown(input: &str, assets: &HashMap<String, String>) -> String {
    compact_code_blocks(&render_markdown(input, assets))
}

pub fn image_urls(input: &str) -> Vec<String> {
    let mut urls = Vec::new();
    collect_image_urls(&parse(input), &mut urls);
    urls
}

fn collect_image_urls(nodes: &[MarkupNode], urls: &mut Vec<String>) {
    for node in nodes {
        match node {
            MarkupNode::Image(url) => urls.push(url.clone()),
            MarkupNode::Bold(children)
            | MarkupNode::Italic(children)
            | MarkupNode::Underline(children)
            | MarkupNode::Strike(children)
            | MarkupNode::Quote(children) => collect_image_urls(children, urls),
            MarkupNode::Link { children, .. } => collect_image_urls(children, urls),
            MarkupNode::Text(_)
            | MarkupNode::Code(_)
            | MarkupNode::LineBreak
            | MarkupNode::HorizontalRule => {}
        }
    }
}

fn parse_normalized(input: &str) -> Vec<MarkupNode> {
    parse_range(input, 0, None, 0).0
}

fn render_nodes_to_string(nodes: &[MarkupNode], assets: &HashMap<String, String>) -> String {
    let mut output = String::new();
    render_nodes(nodes, assets, &mut output);
    output
}

fn render_blockquote(value: &str) -> String {
    let mut output = String::new();
    for line in value.trim_matches('\n').lines() {
        output.push_str("> ");
        output.push_str(line);
        output.push('\n');
    }
    output
}

fn split_nga_reply_quote(input: &str) -> Option<(&str, &str)> {
    let trimmed_start = input.trim_start();
    if !trimmed_start.starts_with("[pid=") && !trimmed_start.starts_with("[pid]") {
        return None;
    }
    let pid_offset = input.len() - trimmed_start.len();
    let pid_end = input[pid_offset..].find("[/pid]")? + pid_offset + "[/pid]".len();
    let bold_end = input[pid_end..].find("[/b]")? + pid_end + "[/b]".len();
    let mut body_start = bold_end;
    while input[body_start..].starts_with("[br]") {
        body_start += "[br]".len();
    }
    let newline = input[body_start..].find('\n')? + body_start;
    if newline <= body_start || newline + 1 >= input.len() {
        return None;
    }
    Some((&input[..newline], &input[newline + 1..]))
}

fn normalize_source_markup(input: &str) -> String {
    input
        .replace("<br />", "[br]")
        .replace("<br/>", "[br]")
        .replace("<br>", "[br]")
        .replace("<strong>", "[b]")
        .replace("</strong>", "[/b]")
        .replace("<b>", "[b]")
        .replace("</b>", "[/b]")
}

fn parse_range(
    input: &str,
    mut offset: usize,
    closing: Option<&str>,
    depth: usize,
) -> (Vec<MarkupNode>, usize) {
    let mut nodes = Vec::new();
    let mut text_start = offset;
    while offset < input.len() {
        let Some(relative) = input[offset..].find('[') else {
            push_text(&mut nodes, &input[text_start..]);
            return (nodes, input.len());
        };
        let open = offset + relative;
        let Some(end_relative) = input[open..].find(']') else {
            push_text(&mut nodes, &input[text_start..]);
            return (nodes, input.len());
        };
        let end = open + end_relative + 1;
        let tag = &input[open + 1..end - 1];
        if let Some(close) = tag.strip_prefix('/') {
            if closing.is_some_and(|expected| closing_matches(expected, close.trim())) {
                push_text(&mut nodes, &input[text_start..open]);
                return (nodes, end);
            }
            offset = end;
            continue;
        }

        let Some((name, argument)) = recognised_tag(tag) else {
            offset = end;
            continue;
        };
        push_text(&mut nodes, &input[text_start..open]);

        if name == "br" {
            nodes.push(MarkupNode::LineBreak);
            offset = end;
            text_start = end;
            continue;
        }
        if name == "hr" {
            nodes.push(MarkupNode::HorizontalRule);
            offset = end;
            text_start = end;
            continue;
        }
        if depth >= MAX_DEPTH {
            nodes.push(MarkupNode::Text(input[open..end].to_owned()));
            offset = end;
            text_start = end;
            continue;
        }

        if name == "img" {
            let (children, next) = parse_range(input, end, Some("img"), depth + 1);
            let value = plain_text(&children).trim().to_owned();
            if is_safe_url(&value) {
                nodes.push(MarkupNode::Image(value));
            } else {
                nodes.push(MarkupNode::Text(value));
            }
            offset = next;
        } else if name == "code" {
            let close = format!("[/{}]", name);
            if let Some(close_at) = input[end..].find(&close) {
                nodes.push(MarkupNode::Code(input[end..end + close_at].to_owned()));
                offset = end + close_at + close.len();
            } else {
                nodes.push(MarkupNode::Text(input[open..end].to_owned()));
                offset = end;
            }
        } else {
            let (children, next) = parse_range(input, end, Some(name), depth + 1);
            let node = match name {
                "b" => MarkupNode::Bold(children),
                "i" => MarkupNode::Italic(children),
                "u" => MarkupNode::Underline(children),
                "s" => MarkupNode::Strike(children),
                "quote" | "collapse" => MarkupNode::Quote(children),
                "url" => {
                    let url = argument
                        .or_else(|| Some(plain_text(&children)))
                        .unwrap_or_default();
                    if is_safe_url(&url) {
                        MarkupNode::Link { url, children }
                    } else {
                        MarkupNode::Text(plain_text(&children))
                    }
                }
                _ => MarkupNode::Text(plain_text(&children)),
            };
            nodes.push(node);
            offset = next;
        }
        text_start = offset;
    }
    push_text(&mut nodes, &input[text_start..]);
    (nodes, input.len())
}

fn recognised_tag(tag: &str) -> Option<(&str, Option<String>)> {
    let trimmed = tag.trim();
    let (name, argument) = trimmed
        .split_once('=')
        .map_or((trimmed, None), |(name, argument)| {
            (
                name.trim(),
                Some(argument.trim().trim_matches('"').to_owned()),
            )
        });
    let name = name.to_ascii_lowercase();
    let name = match name.as_str() {
        "b" | "strong" => "b",
        "i" | "em" => "i",
        "u" => "u",
        "s" | "strike" | "del" => "s",
        "quote" | "collapse" => "quote",
        "url" => "url",
        "uid" | "pid" => "transparent",
        "img" => "img",
        "code" => "code",
        "br" => "br",
        "hr" => "hr",
        _ => return None,
    };
    Some((name, argument))
}

fn closing_matches(expected: &str, actual: &str) -> bool {
    match expected {
        "b" => matches!(actual, "b" | "strong"),
        "i" => matches!(actual, "i" | "em"),
        "s" => matches!(actual, "s" | "strike" | "del"),
        "quote" => matches!(actual, "quote" | "collapse"),
        "transparent" => matches!(actual, "uid" | "pid"),
        _ => expected == actual,
    }
}

fn push_text(nodes: &mut Vec<MarkupNode>, text: &str) {
    if !text.is_empty() {
        nodes.push(MarkupNode::Text(
            text.replace("\r\n", "\n").replace('\r', "\n"),
        ));
    }
}

fn plain_text(nodes: &[MarkupNode]) -> String {
    let mut output = String::new();
    for node in nodes {
        match node {
            MarkupNode::Text(value) | MarkupNode::Code(value) | MarkupNode::Image(value) => {
                output.push_str(value)
            }
            MarkupNode::LineBreak | MarkupNode::HorizontalRule => output.push('\n'),
            MarkupNode::Bold(children)
            | MarkupNode::Italic(children)
            | MarkupNode::Underline(children)
            | MarkupNode::Strike(children)
            | MarkupNode::Quote(children) => output.push_str(&plain_text(children)),
            MarkupNode::Link { children, .. } => output.push_str(&plain_text(children)),
        }
    }
    output
}

fn render_nodes(nodes: &[MarkupNode], assets: &HashMap<String, String>, output: &mut String) {
    for node in nodes {
        match node {
            MarkupNode::Text(value) => output.push_str(&escape_text(value)),
            MarkupNode::Bold(children) => {
                output.push_str("**");
                render_nodes(children, assets, output);
                output.push_str("**");
            }
            MarkupNode::Italic(children) => {
                output.push('*');
                render_nodes(children, assets, output);
                output.push('*');
            }
            MarkupNode::Underline(children) => {
                output.push_str("<u>");
                render_nodes(children, assets, output);
                output.push_str("</u>");
            }
            MarkupNode::Strike(children) => {
                output.push_str("~~");
                render_nodes(children, assets, output);
                output.push_str("~~");
            }
            MarkupNode::Quote(children) => {
                let mut nested = String::new();
                render_nodes(children, assets, &mut nested);
                for line in nested.trim_matches('\n').lines() {
                    output.push_str("> ");
                    output.push_str(line);
                    output.push('\n');
                }
                output.push('\n');
            }
            MarkupNode::Link { url, children } => {
                output.push('[');
                render_nodes(children, assets, output);
                output.push_str("](");
                output.push_str(url);
                output.push(')');
            }
            MarkupNode::Image(url) => {
                let target = assets.get(url).map(String::as_str).unwrap_or(url);
                output.push_str("![image](");
                output.push_str(target);
                output.push(')');
            }
            MarkupNode::Code(value) => {
                output.push_str("\n```\n");
                output.push_str(value.trim_matches('\n'));
                output.push_str("\n```\n");
            }
            MarkupNode::LineBreak => output.push('\n'),
            MarkupNode::HorizontalRule => output.push_str("\n---\n"),
        }
    }
}

fn render_html_nodes(nodes: &[MarkupNode], output: &mut String) {
    for node in nodes {
        match node {
            MarkupNode::Text(value) => output.push_str(&escape_html_text(value)),
            MarkupNode::Bold(children) => {
                output.push_str("<strong>");
                render_html_nodes(children, output);
                output.push_str("</strong>");
            }
            MarkupNode::Italic(children) => {
                output.push_str("<em>");
                render_html_nodes(children, output);
                output.push_str("</em>");
            }
            MarkupNode::Underline(children) => {
                output.push_str("<u>");
                render_html_nodes(children, output);
                output.push_str("</u>");
            }
            MarkupNode::Strike(children) => {
                output.push_str("<del>");
                render_html_nodes(children, output);
                output.push_str("</del>");
            }
            MarkupNode::Quote(children) => {
                output.push_str("<blockquote>");
                render_html_nodes(children, output);
                output.push_str("</blockquote>");
            }
            MarkupNode::Link { url, children } => {
                output.push_str("<a href=\"");
                output.push_str(&escape_html_attribute(url));
                output.push_str("\" target=\"_blank\" rel=\"noopener noreferrer\">");
                render_html_nodes(children, output);
                output.push_str("</a>");
            }
            MarkupNode::Image(url) => {
                output.push_str("<img src=\"");
                output.push_str(&escape_html_attribute(url));
                output.push_str(
                    "\" alt=\"帖子图片\" loading=\"lazy\" referrerpolicy=\"no-referrer\">",
                );
            }
            MarkupNode::Code(value) => {
                output.push_str("<pre><code>");
                output.push_str(&escape_html_text(value));
                output.push_str("</code></pre>");
            }
            MarkupNode::LineBreak => output.push_str("<br>"),
            MarkupNode::HorizontalRule => output.push_str("<hr>"),
        }
    }
}

fn escape_html_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn escape_html_attribute(value: &str) -> String {
    escape_html_text(value).replace(['\n', '\r'], "")
}

fn escape_text(value: &str) -> String {
    value.replace('\\', "\\\\").replace("\t", "    ")
}

fn compact_code_blocks(value: &str) -> String {
    let mut output = Vec::new();
    let mut code_lines = Vec::new();
    let mut in_code = false;

    for line in value.lines() {
        if line.trim() == "```" {
            if in_code {
                if !is_meaningless_code_block(&code_lines) {
                    output.append(&mut code_lines);
                } else {
                    code_lines.clear();
                }
                in_code = false;
            } else {
                in_code = true;
            }
        } else if in_code {
            code_lines.push(line.to_owned());
        } else {
            output.push(line.to_owned());
        }
    }

    // Keep malformed/unclosed blocks visible rather than dropping user text.
    if in_code {
        output.extend(code_lines);
    }

    let mut compacted = String::new();
    let mut previous_blank = false;
    for line in output {
        let blank = line.trim().is_empty();
        if blank && previous_blank {
            continue;
        }
        if !compacted.is_empty() {
            compacted.push('\n');
        }
        compacted.push_str(&line);
        previous_blank = blank;
    }
    normalise_output(&compacted)
}

fn is_meaningless_code_block(lines: &[String]) -> bool {
    let meaningful_lines = lines
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty());
    let mut has_content = false;
    for line in meaningful_lines {
        if !line
            .chars()
            .all(|character| matches!(character, '-' | '=' | '_' | '*' | '`' | ' '))
        {
            has_content = true;
        }
    }
    !has_content
}

fn normalise_output(value: &str) -> String {
    let mut output = value.trim().to_owned();
    output.push('\n');
    output
}

fn is_safe_url(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://"))
        && !value.contains(['\n', '\r', '"', '<', '>'])
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{MarkupNode, parse, render_compact_markdown, render_html, render_markdown};

    #[test]
    fn parses_nested_formatting_and_links() {
        let nodes =
            parse("[strong]中文 [em]重点[/em][/strong] [url=https://example.com]链接[/url]");
        assert!(matches!(nodes[0], MarkupNode::Bold(_)));
        assert!(render_markdown("[b]中文[/b]", &HashMap::new()).contains("**中文**"));
        assert!(
            render_markdown("[url=https://example.com]链接[/url]", &HashMap::new())
                .contains("[链接](https://example.com)")
        );
        assert!(render_markdown("[collapse]折叠[/collapse]", &HashMap::new()).contains("> 折叠"));
    }

    #[test]
    fn renders_quotes_images_code_and_rejects_unsafe_links() {
        let mut assets = HashMap::new();
        assets.insert(
            "https://img.nga.cn/a.jpg".to_owned(),
            "assets/a.jpg".to_owned(),
        );
        let output = render_markdown(
            "[quote]引用[/quote][img]https://img.nga.cn/a.jpg[/img][code]a < b[/code][url=javascript:alert(1)]bad[/url]",
            &assets,
        );
        assert!(output.contains("> 引用"));
        assert!(output.contains("![image](assets/a.jpg)"));
        assert!(output.contains("```\na < b\n```"));
        assert!(!output.contains("javascript:"));
    }

    #[test]
    fn renders_safe_admin_html_without_allowing_raw_html_or_script_urls() {
        let output = render_html(
            "<script>alert(1)</script>[b]粗体[/b][quote]引用[/quote][url=javascript:alert(1)]bad[/url][url=https://example.com?a=1&b=2]ok[/url][img]https://img.nga.cn/a.jpg[/img]",
        );
        assert!(output.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(output.contains("<strong>粗体</strong>"));
        assert!(output.contains("<blockquote>引用</blockquote>"));
        assert!(!output.contains("javascript:"));
        assert!(output.contains("href=\"https://example.com?a=1&amp;b=2\""));
        assert!(output.contains("src=\"https://img.nga.cn/a.jpg\""));
    }

    #[test]
    fn preserves_plain_text_and_line_breaks() {
        assert_eq!(
            render_markdown("中文\r\n第二行[br]第三行", &HashMap::new()),
            "中文\n第二行\n第三行\n"
        );
    }

    #[test]
    fn compacts_code_blocks_and_preserves_nga_emoticons() {
        let output = render_compact_markdown(
            "[code]\n---\n[/code][code]重要提示[s:ac:瞎][/code]正文",
            &HashMap::new(),
        );
        assert_eq!(output, "重要提示[s:ac:瞎]\n正文\n");
        assert!(!output.contains("```"));
    }

    #[test]
    fn separates_nga_pid_quote_from_the_current_reply() {
        let input = "[pid=876581497,47264819,1]Reply[/pid] <b>Post by [uid=998781]组我准灭团[/uid] (2026-07-27 22:10):</b><br/><br/>[img]https://img.nga.cn/a.jpg[/img]都吃夜宵了还在意健不健康嘛[s:ac:瞎]\n夜宵还得你这个[s:ac:哭笑]";
        let output = render_markdown(input, &HashMap::new());
        assert!(output.contains("> Reply **Post by 组我准灭团 (2026-07-27 22:10):**"));
        assert!(output.contains("都吃夜宵了还在意健不健康嘛[s:ac:瞎]"));
        assert!(
            output.contains("嘛[s:ac:瞎]\n\n夜宵还得你这个[s:ac:哭笑]"),
            "rendered output: {output:?}"
        );
        assert!(!output.contains("<br/>") && !output.contains("[/uid]"));
    }

    #[test]
    fn leaves_a_blank_line_after_nga_quote_blocks() {
        let input = "[quote][pid=876581497,47264819,1]Reply[/pid] <b>Post by [uid=998781]组我准灭团[/uid]:</b><br/><br/>[img]https://img.nga.cn/a.jpg[/img]都吃夜宵了还在意健不健康嘛[s:ac:瞎][/quote]夜宵还得你这个[s:ac:哭笑]";
        let output = render_markdown(input, &HashMap::new());
        assert!(output.contains("> ![image](https://img.nga.cn/a.jpg)都吃夜宵了还在意健不健康嘛[s:ac:瞎]\n\n夜宵还得你这个"));
    }
}
