package content

import (
	"html"
	"html/template"
	"net/url"
	"regexp"
	"strings"
)

type node struct {
	tag, param, text string
	children         []*node
}

var tagPattern = regexp.MustCompile(`\[(/?)([a-zA-Z]+)(?:=([^\]]*))?\]`)
var breakPattern = regexp.MustCompile(`(?i)<br\s*/?>`)

// 一个小型标记树共享给 Web、Markdown 和通知；未知标记作为普通文本保留。
func parse(body string) *node {
	body = breakPattern.ReplaceAllString(body, "\n")
	root := &node{}
	stack := []*node{root}
	for len(body) > 0 {
		parent := stack[len(stack)-1]
		match := tagPattern.FindStringSubmatchIndex(body)
		if match == nil {
			parent.children = append(parent.children, &node{text: body})
			break
		}
		if match[0] > 0 {
			parent.children = append(parent.children, &node{text: body[:match[0]]})
		}
		literal := body[match[0]:match[1]]
		tag := strings.ToLower(body[match[4]:match[5]])
		param := ""
		if match[6] >= 0 {
			param = body[match[6]:match[7]]
		}
		closing := body[match[2]:match[3]] == "/"
		body = body[match[1]:]
		supported := false
		switch tag {
		case "b", "i", "u", "s", "del", "quote", "code", "url", "img", "collapse", "color", "size":
			supported = true
		}
		if !supported || len(stack) > 64 {
			parent.children = append(parent.children, &node{text: literal})
			continue
		}
		if closing {
			if len(stack) > 1 && parent.tag == tag {
				stack = stack[:len(stack)-1]
			} else {
				parent.children = append(parent.children, &node{text: literal})
			}
			continue
		}
		current := &node{tag: tag, param: param}
		parent.children = append(parent.children, current)
		if tag == "code" {
			end := strings.Index(strings.ToLower(body), "[/code]")
			if end < 0 {
				end = len(body)
			}
			current.text = body[:end]
			body = body[end:]
			if strings.HasPrefix(strings.ToLower(body), "[/code]") {
				body = body[7:]
			}
			continue
		}
		stack = append(stack, current)
	}
	return root
}
func SafeURL(raw string) string {
	raw = strings.TrimSpace(html.UnescapeString(raw))
	if strings.HasPrefix(raw, "//") {
		raw = "https:" + raw
	}
	u, err := url.Parse(raw)
	if err != nil || u.Host == "" || u.User != nil || (u.Scheme != "http" && u.Scheme != "https") {
		return ""
	}
	return u.String()
}
func plain(n *node) string {
	var b strings.Builder
	b.WriteString(n.text)
	for _, c := range n.children {
		b.WriteString(plain(c))
	}
	return b.String()
}
func Text(body string) string        { return strings.TrimSpace(plain(parse(body))) }
func HTML(body string) template.HTML { return template.HTML(render(parse(body), "html", nil)) }
func HTMLWithResources(body string, urls []string) template.HTML {
	return template.HTML(render(parse(body), "html", ResourceAliases(urls, nil)))
}
func ResourceAliases(urls []string, local map[string]string) map[string]string {
	result := map[string]string{}
	for _, raw := range urls {
		safe := SafeURL(raw)
		if safe == "" {
			continue
		}
		target := safe
		if replacement, ok := local[raw]; ok {
			target = replacement
		}
		result[safe] = target
		u, _ := url.Parse(safe)
		result[u.Path] = target
		result[strings.TrimPrefix(u.Path, "/")] = target
		result[strings.TrimPrefix(u.Path, "/attachments/")] = target
	}
	return result
}
func Destination(target string) string {
	return strings.NewReplacer("(", "%28", ")", "%29", "<", "%3C", ">", "%3E", "\"", "%22", " ", "%20").Replace(target)
}
func Markdown(body string, resources map[string]string) string {
	return render(parse(body), "markdown", resources)
}
func EscapeMarkdown(text string) string {
	return strings.NewReplacer("\\", "\\\\", "`", "\\`", "*", "\\*", "_", "\\_", "[", "\\[", "]", "\\]", "<", "&lt;", ">", "&gt;", "#", "\\#", "|", "\\|").Replace(text)
}
func render(n *node, format string, resources map[string]string) string {
	htmlMode := format == "html"
	escape := EscapeMarkdown
	if htmlMode {
		escape = html.EscapeString
	}
	var b strings.Builder
	text := escape(n.text)
	if htmlMode && n.tag != "code" {
		text = strings.ReplaceAll(text, "\n", "<br>\n")
	}
	b.WriteString(text)
	for _, c := range n.children {
		b.WriteString(render(c, format, resources))
	}
	inside := b.String()
	switch n.tag {
	case "":
		return inside
	case "b":
		if htmlMode {
			return "<strong>" + inside + "</strong>"
		}
		return "**" + inside + "**"
	case "i":
		if htmlMode {
			return "<em>" + inside + "</em>"
		}
		return "*" + inside + "*"
	case "u":
		if htmlMode {
			return "<u>" + inside + "</u>"
		}
		return inside
	case "s", "del":
		if htmlMode {
			return "<del>" + inside + "</del>"
		}
		return "~~" + inside + "~~"
	case "quote":
		if htmlMode {
			return "<blockquote>" + inside + "</blockquote>"
		}
		return "\n> " + strings.ReplaceAll(inside, "\n", "\n> ") + "\n"
	case "code":
		if htmlMode {
			return "<pre><code>" + html.EscapeString(n.text) + "</code></pre>"
		}
		fence := "```"
		for strings.Contains(n.text, fence) {
			fence += "`"
		}
		return "\n" + fence + "\n" + n.text + "\n" + fence + "\n"
	case "collapse":
		title := n.param
		if title == "" {
			title = "折叠内容"
		}
		if htmlMode {
			return "<details><summary>" + escape(title) + "</summary>" + inside + "</details>"
		}
		return "\n**" + escape(title) + "**\n\n" + inside + "\n"
	case "url", "img":
		target := n.param
		if target == "" || n.tag == "img" {
			target = plain(n)
		}
		rawTarget := strings.TrimSpace(html.UnescapeString(target))
		target = SafeURL(rawTarget)
		if target == "" {
			target = resources[strings.TrimPrefix(rawTarget, "./")]
		}
		if target == "" {
			return inside
		}
		if replacement, ok := resources[target]; ok {
			target = replacement
		}
		if htmlMode {
			if n.tag == "img" {
				return `<img loading="lazy" referrerpolicy="no-referrer" alt="图片" src="` + html.EscapeString(target) + `">`
			}
			return `<a rel="noreferrer" target="_blank" href="` + html.EscapeString(target) + `">` + inside + `</a>`
		}
		target = strings.NewReplacer("(", "%28", ")", "%29", "<", "%3C", ">", "%3E", "\"", "%22", " ", "%20").Replace(target)
		if n.tag == "img" {
			return "![图片](" + target + ")"
		}
		return "[" + inside + "](" + target + ")"
	}
	return inside
}
