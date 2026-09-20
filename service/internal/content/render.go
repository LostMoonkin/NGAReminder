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

// Web 和纯文本摘要使用此解析器；Markdown 导出与飞书卡片各有独立入口。
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
func HTML(body string) template.HTML { return template.HTML(renderHTML(parse(body), nil)) }
func HTMLWithResources(body string, urls []string) template.HTML {
	return template.HTML(renderHTML(parse(body), ResourceAliases(urls, nil)))
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
	return strings.NewReplacer("\\", "%5C", "(", "%28", ")", "%29", "<", "%3C", ">", "%3E", "\"", "%22", " ", "%20").Replace(target)
}
func EscapeMarkdown(text string) string {
	return strings.NewReplacer("\\", "\\\\", "`", "\\`", "*", "\\*", "_", "\\_", "[", "\\[", "]", "\\]", "<", "&lt;", ">", "&gt;", "#", "\\#", "|", "\\|").Replace(text)
}
func renderHTML(n *node, resources map[string]string) string {
	var b strings.Builder
	text := html.EscapeString(n.text)
	if n.tag != "code" {
		text = strings.ReplaceAll(text, "\n", "<br>\n")
	}
	b.WriteString(text)
	for _, c := range n.children {
		b.WriteString(renderHTML(c, resources))
	}
	inside := b.String()
	switch n.tag {
	case "":
		return inside
	case "b":
		return "<strong>" + inside + "</strong>"
	case "i":
		return "<em>" + inside + "</em>"
	case "u":
		return "<u>" + inside + "</u>"
	case "s", "del":
		return "<del>" + inside + "</del>"
	case "quote":
		return "<blockquote>" + inside + "</blockquote>"
	case "code":
		return "<pre><code>" + html.EscapeString(n.text) + "</code></pre>"
	case "collapse":
		title := n.param
		if title == "" {
			title = "折叠内容"
		}
		return "<details><summary>" + html.EscapeString(title) + "</summary>" + inside + "</details>"
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
		if n.tag == "img" {
			return `<img loading="lazy" referrerpolicy="no-referrer" alt="图片" src="` + html.EscapeString(target) + `">`
		}
		return `<a rel="noreferrer" target="_blank" href="` + html.EscapeString(target) + `">` + inside + `</a>`
	}
	return inside
}
