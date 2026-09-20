package content

// NGA 特殊格式的输出语义参考 ludoux/ngapost2md 的 fixMost/processMedia，
// 提交 e3b94346c805ac851ce2584e5ab4e3735846a3c9；许可证见 licenses/ngapost2md.txt。
// 这里仅转换已保存正文，不执行上游的原文补全、网络请求或文件下载。
import (
	"fmt"
	"html"
	"regexp"
	"strconv"
	"strings"

	nethtml "golang.org/x/net/html"
)

type PostReference struct{ TID, PID int64 }

func (r PostReference) Anchor() string { return fmt.Sprintf("tid%d-pid%d", r.TID, r.PID) }

func (r PostReference) URL() string {
	if r.TID <= 0 || r.PID < 0 {
		return ""
	}
	url := fmt.Sprintf("https://bbs.nga.cn/read.php?tid=%d", r.TID)
	if r.PID > 0 {
		url += fmt.Sprintf("&pid=%d", r.PID)
	}
	return url
}

// MarkdownBody 只持有一条正文；导出方可先核对引用是否在本次导出中，再渲染。
type MarkdownBody struct {
	References []PostReference
	root       *markdownNode
}

type markdownNode struct {
	tag, param, text, closing string
	children                  []*markdownNode
	reference                 *PostReference
}

type markdownFrame struct {
	key  string
	node *markdownNode
}

type markdownParser struct {
	MarkdownBody
	tid   int64
	stack []markdownFrame
}

var (
	markdownToken = regexp.MustCompile(`(?i)\[/?[a-z]+(?:[:=][^\]\r\n]*)?\]|<`)
	codeOpening   = regexp.MustCompile(`(?is)\[code(?:=[^\]]*)?\]|<pre\b[^>]*>|<code\b[^>]*>`)
	preCodeWrap   = regexp.MustCompile(`(?is)^\s*<code\b[^>]*>(.*?)</code>\s*$`)
	quoteHeader   = regexp.MustCompile(`(?is)^\[(pid|tid)=(\d+)(?:,(\d+))?(?:,[^\]]*)?\][^\[]*\[/(?:pid|tid)\]\s*(?:<b\b[^>]*>|\[b\])?Post by ([^\r\n]*?)\s+\((\d{4}-\d{2}-\d{2} \d{2}:\d{2}(?::\d{2})?)\):(?:</b>|\[/b\])?`)
	replyHeader   = regexp.MustCompile(`(?is)^(?:<b\b[^>]*>|\[b\])Reply to \[(pid|tid)=(\d+)(?:,(\d+))?(?:,[^\]]*)?\][^\[]*\[/(?:pid|tid)\]\s*Post by ([^\r\n]*?)\s+\((\d{4}-\d{2}-\d{2} \d{2}:\d{2}(?::\d{2})?)\)(?:</b>|\[/b\])`)
	quoteAuthor   = regexp.MustCompile(`(?is)^\[uid=(-?\d+)\](.*?)\[/uid\](.*)$`)
	diceResult    = regexp.MustCompile(`(?s)^ROLL\s*:\s*(.+?)=(.+?)=(.+)$`)
	ngaEscapes    = strings.NewReplacer(`\u0026`, "&", `\u003c`, "<", `\u003e`, ">", "&amp;#160;", " ", "&lt;br/&gt;", "\n", "&lt;br&gt;", "\n")
)

func ParseMarkdown(body string, tid int64) MarkdownBody {
	p := markdownParser{tid: tid}
	p.root = &markdownNode{}
	p.stack = []markdownFrame{{node: p.root}}
	// 先隔离代码，避免实体、匿名编码和 HTML/BBCode 转换改变代码示例。
	for {
		loc := codeOpening.FindStringIndex(body)
		if loc == nil {
			p.consume(ngaEscapes.Replace(body))
			break
		}
		p.consume(ngaEscapes.Replace(body[:loc[0]]))
		opening := strings.ToLower(body[loc[0]:loc[1]])
		body = body[loc[1]:]
		end := "[/code]"
		if strings.HasPrefix(opening, "<pre") {
			end = "</pre>"
		} else if strings.HasPrefix(opening, "<code") {
			end = "</code>"
		}
		i := strings.Index(strings.ToLower(body), end)
		code := body
		if i >= 0 {
			code, body = body[:i], body[i+len(end):]
		} else {
			body = ""
		}
		if strings.HasPrefix(opening, "<") {
			if match := preCodeWrap.FindStringSubmatch(code); match != nil {
				code = match[1]
			}
			code = html.UnescapeString(code)
		}
		p.append(&markdownNode{tag: "code", text: code})
	}
	return p.MarkdownBody
}

func (p *markdownParser) append(n *markdownNode) {
	parent := p.stack[len(p.stack)-1].node
	parent.children = append(parent.children, n)
}

func (p *markdownParser) text(text string) {
	if text != "" {
		p.append(&markdownNode{text: html.UnescapeString(text)})
	}
}

func (p *markdownParser) open(key string, n *markdownNode) {
	p.append(n)
	p.stack = append(p.stack, markdownFrame{key, n})
}

func (p *markdownParser) close(key, literal string) {
	// HTML 的同名容器可能嵌套；只关闭当前匹配项，残缺标记保留为文字。
	if len(p.stack) > 1 && p.stack[len(p.stack)-1].key == key {
		n := p.stack[len(p.stack)-1].node
		if n.tag == "literal" {
			n.closing = literal
		}
		p.stack = p.stack[:len(p.stack)-1]
	} else {
		p.text(literal)
	}
}

func (p *markdownParser) reference(match []string) *markdownNode {
	a, errA := strconv.ParseInt(match[2], 10, 64)
	b, errB := strconv.ParseInt(match[3], 10, 64)
	r := PostReference{TID: p.tid, PID: a}
	if strings.EqualFold(match[1], "tid") {
		r.TID, r.PID = a, 0
	} else if match[3] != "" {
		r.TID = b
	}
	if errA != nil || match[3] != "" && errB != nil {
		r = PostReference{}
	}
	author := stripHTML(match[4])
	if user := quoteAuthor.FindStringSubmatch(author); user != nil {
		author = user[2] + "(" + user[1] + ")" + user[3]
	}
	n := &markdownNode{tag: "reference", text: AnonymousName(author) + "(" + match[5] + ")", reference: &r}
	if r.URL() != "" {
		p.References = append(p.References, r)
	}
	return n
}

func stripHTML(raw string) string {
	z := nethtml.NewTokenizer(strings.NewReader(raw))
	var b strings.Builder
	for {
		switch z.Next() {
		case nethtml.ErrorToken:
			return b.String()
		case nethtml.TextToken:
			b.Write(z.Text())
		}
	}
}

func (p *markdownParser) consume(body string) {
	for body != "" {
		loc := markdownToken.FindStringIndex(body)
		if loc == nil {
			p.text(body)
			return
		}
		p.text(body[:loc[0]])
		body = body[loc[0]:]
		if len(p.stack) > 64 {
			p.text(body)
			return
		}
		if match := replyHeader.FindStringSubmatch(body); match != nil {
			n := &markdownNode{tag: "quote", children: []*markdownNode{p.reference(match)}}
			p.append(n)
			body = body[len(match[0]):]
			continue
		}
		if body[0] == '<' {
			z := nethtml.NewTokenizer(strings.NewReader(body))
			kind := z.Next()
			raw := string(z.Raw())
			if raw == "" {
				p.text(body)
				return
			}
			if kind == nethtml.TextToken {
				// 普通小于号不是 HTML，后续文字仍可能包含 BBCode。
				p.text("<")
				body = body[1:]
				continue
			}
			body = body[len(raw):]
			if kind == nethtml.StartTagToken || kind == nethtml.EndTagToken || kind == nethtml.SelfClosingTagToken {
				p.htmlToken(z.Token(), kind, raw)
			} else {
				p.text(raw)
			}
			continue
		}
		end := strings.IndexByte(body, ']') + 1
		raw := body[:end]
		body = body[end:]
		if filename, ok := ngaSmiles[raw]; ok {
			label := strings.TrimSuffix(raw[strings.LastIndexByte(raw, ':')+1:], "]")
			p.append(&markdownNode{tag: "smile", text: label, param: "https://img4.nga.178.com/ngabbs/post/smile/" + filename})
			continue
		}
		inner := raw[1 : len(raw)-1]
		closing := strings.HasPrefix(inner, "/")
		inner = strings.TrimPrefix(inner, "/")
		tag, param, _ := strings.Cut(inner, "=")
		tag = strings.ToLower(tag)
		switch tag {
		case "b", "i", "u", "s", "del", "quote", "url", "img", "collapse", "color", "size", "audio", "video":
		default:
			p.text(raw)
			continue
		}
		if closing {
			p.close("bb:"+tag, raw)
			continue
		}
		n := &markdownNode{tag: tag, param: html.UnescapeString(param)}
		if tag == "audio" || tag == "video" {
			// BBCode 的可选参数是播放属性；地址来自标签正文。
			n.param = ""
		}
		p.open("bb:"+tag, n)
		if tag == "quote" {
			if match := quoteHeader.FindStringSubmatch(body); match != nil {
				n.children = append(n.children, p.reference(match))
				body = body[len(match[0]):]
			}
		}
	}
}

func (p *markdownParser) htmlToken(t nethtml.Token, kind nethtml.TokenType, raw string) {
	if kind == nethtml.EndTagToken {
		p.close("html:"+t.Data, raw)
		return
	}
	attrs := map[string]string{}
	for _, a := range t.Attr {
		attrs[a.Key] = a.Val
	}
	n := &markdownNode{}
	void := false
	switch t.Data {
	case "b", "strong":
		n.tag = "b"
	case "i", "em":
		n.tag = "i"
	case "u", "s", "del":
		n.tag = t.Data
	case "blockquote":
		n.tag = "quote"
	case "a":
		n.tag, n.param = "url", attrs["href"]
	case "img":
		n.tag, n.param, void = "img", attrs["src"], true
	case "br":
		p.text("\n")
		return
	case "p", "div":
		n.tag = "paragraph"
		classes := " " + attrs["class"] + " "
		switch {
		case strings.Contains(classes, " dice "):
			n.tag = "dice"
		case strings.Contains(classes, " foldBox "):
			n.tag = "collapse"
		case strings.Contains(classes, " collapse_btn "):
			n.tag = "summary"
		}
	case "span":
	case "audio", "video":
		n.tag, n.param = t.Data, attrs["src"]
	case "source":
		parent := p.stack[len(p.stack)-1].node
		if (parent.tag == "video" || parent.tag == "audio") && parent.param == "" {
			parent.param = attrs["src"]
		}
		return
	default:
		n.tag, n.text = "literal", raw
	}
	if void || kind == nethtml.SelfClosingTagToken {
		p.append(n)
	} else {
		p.open("html:"+t.Data, n)
	}
}

func markdownPlain(n *markdownNode) string {
	var b strings.Builder
	b.WriteString(n.text)
	for _, c := range n.children {
		b.WriteString(markdownPlain(c))
	}
	return b.String()
}

func Markdown(body string, resources map[string]string) string {
	return ParseMarkdown(body, 0).Render(resources, nil)
}

func (b MarkdownBody) Render(resources map[string]string, links map[PostReference]string) string {
	return renderMarkdown(b.root, resources, links)
}

func renderMarkdown(n *markdownNode, resources map[string]string, links map[PostReference]string) string {
	if n.tag == "code" {
		fence := "```"
		for strings.Contains(n.text, fence) {
			fence += "`"
		}
		return "\n" + fence + "\n" + n.text + "\n" + fence + "\n"
	}
	if n.tag == "reference" {
		link := n.reference.URL()
		if local := links[*n.reference]; local != "" {
			link = local
		}
		heading := EscapeMarkdown(n.text) + " 说: "
		if link != "" {
			heading = "[jump](" + Destination(link) + ") " + heading
		}
		return heading
	}
	var out strings.Builder
	out.WriteString(EscapeMarkdown(AnonymousName(n.text)))
	for _, child := range n.children {
		if n.tag == "collapse" && child.tag == "summary" {
			continue
		}
		out.WriteString(renderMarkdown(child, resources, links))
	}
	inside := out.String()
	switch n.tag {
	case "b":
		return "**" + inside + "**"
	case "i":
		return "*" + inside + "*"
	case "s", "del":
		return "~~" + inside + "~~"
	case "quote":
		return "\n> " + strings.ReplaceAll(strings.TrimSpace(inside), "\n", "\n> ") + "\n\n"
	case "paragraph":
		return "\n\n" + inside + "\n\n"
	case "collapse":
		title := n.param
		for _, child := range n.children {
			if child.tag == "summary" {
				title = strings.TrimSuffix(strings.TrimPrefix(strings.TrimSpace(markdownPlain(child)), "+"), " ...")
			}
		}
		if title == "" {
			title = "折叠内容"
		}
		return "\n<details>\n<summary>" + html.EscapeString(AnonymousName(title)) + "</summary>\n\n" + inside + "\n\n</details>\n"
	case "dice":
		if match := diceResult.FindStringSubmatch(strings.TrimSpace(markdownPlain(n))); match != nil {
			return " **【ROLL** : " + EscapeMarkdown(match[1]) + "= **" + EscapeMarkdown(match[3]) + "】** "
		}
	case "smile":
		return "![" + EscapeMarkdown(n.text) + "](" + n.param + ")"
	case "url", "img", "audio", "video":
		raw := n.param
		if raw == "" || n.tag == "img" && len(n.children) > 0 {
			raw = markdownPlain(n)
		}
		target := markdownResource(raw, n.tag == "img", resources)
		if target == "" {
			return inside
		}
		target = Destination(target)
		switch n.tag {
		case "img":
			return "![img](" + target + ")"
		case "audio":
			return "【音频：" + target + "】"
		case "video":
			return "【视频：" + target + "】"
		}
		if inside == "" {
			inside = EscapeMarkdown(raw)
		}
		return "[" + inside + "](" + target + ")"
	case "literal":
		return inside + EscapeMarkdown(n.closing)
	}
	return inside
}

func imageURL(raw string) string {
	if strings.HasPrefix(raw, "./") {
		raw = "https://img.nga.178.com/attachments/" + strings.TrimPrefix(raw, "./")
	}
	return strings.ReplaceAll(raw, ".medium.jpg", "")
}

func markdownResource(raw string, isImage bool, resources map[string]string) string {
	raw = strings.TrimSpace(html.UnescapeString(raw))
	candidates := []string{raw, strings.TrimPrefix(raw, "./"), SafeURL(raw)}
	if isImage {
		candidates = append(candidates, imageURL(raw), strings.TrimPrefix(imageURL(raw), "./"))
	}
	for _, key := range candidates {
		if target, ok := resources[key]; ok {
			if strings.HasPrefix(target, "assets/") {
				return target
			}
			if isImage {
				target = imageURL(target)
			}
			return SafeURL(target)
		}
	}
	if isImage {
		raw = imageURL(raw)
	}
	return SafeURL(raw)
}

// 图片原地址与归一化地址都指向同一份已保存资源，Web 继续使用原来的映射。
func MarkdownResourceAliases(urls []string, local map[string]string) map[string]string {
	aliases := ResourceAliases(urls, local)
	for key, target := range ResourceAliases(urls, local) {
		if _, exists := aliases[imageURL(key)]; !exists {
			aliases[imageURL(key)] = target
		}
	}
	return aliases
}
