package content

import (
	"strings"
	"unicode"
)

// Feishu 沿用 Rust markup::render_compact_markdown 与通知 sender 的规则。
// 飞书卡片独立于 Web 摘要和 ngapost2md 导出，避免后两者改变已确认的卡片格式。
func Feishu(body string) (text string, images []string) {
	var plain strings.Builder
	images = []string{}
	for {
		start := strings.Index(body, "[img]")
		if start < 0 {
			break
		}
		plain.WriteString(body[:start])
		after := body[start+5:]
		end := strings.Index(after, "[/img]")
		if end < 0 {
			body = body[start:]
			break
		}
		if source := strings.TrimSpace(after[:end]); source != "" {
			images = append(images, source)
		}
		body = after[end+6:]
	}
	plain.WriteString(body)
	body = strings.NewReplacer("<br />", "[br]", "<br/>", "[br]", "<br>", "[br]",
		"<strong>", "[b]", "</strong>", "[/b]", "<b>", "[b]", "</b>", "[/b]").Replace(strings.TrimSpace(plain.String()))
	if quoted, rest, ok := splitFeishuReplyQuote(body); ok {
		text = feishuBlockquote(renderFeishuMarkup(quoted)) + "\n" + renderFeishuMarkup(rest)
	} else {
		text = renderFeishuMarkup(body)
	}
	text = compactFeishuCode(strings.TrimSpace(text) + "\n")
	runes := []rune(text)
	if len(runes) > 2000 {
		const suffix = "\n\n内容较长，点击“查看帖子”查看完整内容。"
		text = string(runes[:2000-len([]rune(suffix))-1]) + "…" + suffix
	}
	return text, images
}

func splitFeishuReplyQuote(input string) (string, string, bool) {
	trimmed := strings.TrimLeftFunc(input, unicode.IsSpace)
	if !strings.HasPrefix(trimmed, "[pid=") && !strings.HasPrefix(trimmed, "[pid]") {
		return "", "", false
	}
	pidEnd := strings.Index(trimmed, "[/pid]")
	if pidEnd < 0 {
		return "", "", false
	}
	pidEnd += len(input) - len(trimmed) + 6
	boldEnd := strings.Index(input[pidEnd:], "[/b]")
	if boldEnd < 0 {
		return "", "", false
	}
	start := pidEnd + boldEnd + 4
	for strings.HasPrefix(input[start:], "[br]") {
		start += 4
	}
	newline := strings.IndexByte(input[start:], '\n')
	if newline <= 0 || start+newline+1 >= len(input) {
		return "", "", false
	}
	return input[:start+newline], input[start+newline+1:], true
}

func renderFeishuMarkup(input string) string {
	nodes, _ := parseFeishuRange(input, 0, "", 0)
	return renderFeishuNodes(nodes)
}

func feishuTag(raw string) (name, argument string, hasArgument bool) {
	name, argument, hasArgument = strings.Cut(strings.TrimSpace(raw), "=")
	argument = strings.Trim(strings.TrimSpace(argument), `"`)
	switch strings.ToLower(strings.TrimSpace(name)) {
	case "b", "strong":
		name = "b"
	case "i", "em":
		name = "i"
	case "s", "strike", "del":
		name = "s"
	case "quote", "collapse":
		name = "quote"
	case "uid", "pid":
		name = "transparent"
	case "u", "url", "img", "code", "br", "hr":
		name = strings.ToLower(strings.TrimSpace(name))
	default:
		name = ""
	}
	return
}

func feishuClosing(expected, actual string) bool {
	switch expected {
	case "b":
		return actual == "b" || actual == "strong"
	case "i":
		return actual == "i" || actual == "em"
	case "s":
		return actual == "s" || actual == "strike" || actual == "del"
	case "quote":
		return actual == "quote" || actual == "collapse"
	case "transparent":
		return actual == "uid" || actual == "pid"
	default:
		return expected == actual
	}
}

func parseFeishuRange(input string, offset int, closing string, depth int) (nodes []*node, next int) {
	push := func(text string) {
		if text != "" {
			nodes = append(nodes, &node{text: strings.ReplaceAll(strings.ReplaceAll(text, "\r\n", "\n"), "\r", "\n")})
		}
	}
	textStart := offset
	for offset < len(input) {
		open := strings.IndexByte(input[offset:], '[')
		if open < 0 {
			break
		}
		open += offset
		end := strings.IndexByte(input[open:], ']')
		if end < 0 {
			break
		}
		end += open + 1
		raw := input[open+1 : end-1]
		if actual, ok := strings.CutPrefix(raw, "/"); ok {
			if closing != "" && feishuClosing(closing, strings.TrimSpace(actual)) {
				push(input[textStart:open])
				return nodes, end
			}
			offset = end
			continue
		}
		name, argument, hasArgument := feishuTag(raw)
		if name == "" {
			offset = end
			continue
		}
		push(input[textStart:open])
		switch {
		case name == "br" || name == "hr":
			nodes = append(nodes, &node{tag: name})
			offset = end
		case depth >= 64:
			push(input[open:end])
			offset = end
		case name == "code":
			closeAt := strings.Index(input[end:], "[/code]")
			if closeAt < 0 {
				push(input[open:end])
				offset = end
			} else {
				nodes = append(nodes, &node{tag: "code", text: input[end : end+closeAt]})
				offset = end + closeAt + 7
			}
		default:
			children, next := parseFeishuRange(input, end, name, depth+1)
			current := &node{tag: name, children: children}
			switch name {
			case "img":
				value := strings.TrimSpace(feishuPlain(children))
				current = &node{text: value}
				if safeFeishuURL(value) {
					current.tag = "img"
				}
			case "url":
				if !hasArgument {
					argument = feishuPlain(children)
				}
				current.param = argument
				if !safeFeishuURL(argument) {
					current = &node{text: feishuPlain(children)}
				}
			case "transparent":
				current = &node{text: feishuPlain(children)}
			}
			nodes = append(nodes, current)
			offset = next
		}
		textStart = offset
	}
	push(input[textStart:])
	return nodes, len(input)
}

func safeFeishuURL(value string) bool {
	lower := strings.ToLower(strings.TrimSpace(value))
	return (strings.HasPrefix(lower, "https://") || strings.HasPrefix(lower, "http://")) && !strings.ContainsAny(value, "\n\r\"<>")
}

func feishuPlain(nodes []*node) string {
	var out strings.Builder
	for _, current := range nodes {
		if current.tag == "br" || current.tag == "hr" {
			out.WriteByte('\n')
		} else {
			out.WriteString(current.text)
			out.WriteString(feishuPlain(current.children))
		}
	}
	return out.String()
}

func renderFeishuNodes(nodes []*node) string {
	var out strings.Builder
	for _, current := range nodes {
		inside := renderFeishuNodes(current.children)
		switch current.tag {
		case "b":
			out.WriteString("**" + inside + "**")
		case "i":
			out.WriteString("*" + inside + "*")
		case "u":
			out.WriteString("<u>" + inside + "</u>")
		case "s":
			out.WriteString("~~" + inside + "~~")
		case "quote":
			out.WriteString(feishuBlockquote(inside) + "\n")
		case "url":
			out.WriteString("[" + inside + "](" + current.param + ")")
		case "img":
			out.WriteString("![image](" + current.text + ")")
		case "code":
			out.WriteString("\n```\n" + strings.Trim(current.text, "\n") + "\n```\n")
		case "br":
			out.WriteByte('\n')
		case "hr":
			out.WriteString("\n---\n")
		default:
			out.WriteString(strings.NewReplacer("\\", "\\\\", "\t", "    ").Replace(current.text))
		}
	}
	return out.String()
}

func feishuLines(value string) []string {
	if value == "" {
		return nil
	}
	lines := strings.Split(strings.TrimSuffix(value, "\n"), "\n")
	for index := range lines {
		lines[index] = strings.TrimSuffix(lines[index], "\r")
	}
	return lines
}

func feishuBlockquote(value string) string {
	var out strings.Builder
	for _, line := range feishuLines(strings.Trim(value, "\n")) {
		out.WriteString("> " + line + "\n")
	}
	return out.String()
}

func compactFeishuCode(value string) string {
	var output, code []string
	inCode := false
	for _, line := range feishuLines(value) {
		if strings.TrimSpace(line) == "```" {
			if inCode {
				for _, part := range code {
					if strings.Trim(strings.TrimSpace(part), "-=_*` ") != "" {
						output = append(output, code...)
						break
					}
				}
				code = nil
			}
			inCode = !inCode
		} else if inCode {
			code = append(code, line)
		} else {
			output = append(output, line)
		}
	}
	if inCode {
		output = append(output, code...)
	}
	var out strings.Builder
	previousBlank := false
	for _, line := range output {
		blank := strings.TrimSpace(line) == ""
		if blank && previousBlank {
			continue
		}
		if out.Len() > 0 {
			out.WriteByte('\n')
		}
		out.WriteString(line)
		previousBlank = blank
	}
	return strings.TrimSpace(out.String()) + "\n"
}
