package content

import (
	"strings"
	"testing"
)

func TestSharedMarkupAndSafeHTML(t *testing.T) {
	body := "<script>alert(1)</script>\n[b]bold[/b][quote]quoted[/quote][code]<b>literal</b>\n```[/code][url=https://example.invalid/?x=1&y=2]link[/url][url=javascript:alert(1)]unsafe[/url][collapse=details]inside[/collapse][img]./sample.png[/img][unknown]keep[/unknown]"
	resources := []string{"https://img.nga.cn/attachments/sample.png"}
	html := string(HTMLWithResources(body, resources))
	for _, unsafe := range []string{"<script>", `href="javascript:`, `<b>literal</b>`, `<code>&lt;b&gt;literal&lt;/b&gt;<br>`} {
		if strings.Contains(html, unsafe) {
			t.Fatal("unsafe or corrupted HTML", unsafe)
		}
	}
	for _, safe := range []string{"&lt;script&gt;", "<strong>bold</strong>", "<blockquote>", "<pre><code>", "<details>", `src="https://img.nga.cn/attachments/sample.png"`, "[unknown]keep[/unknown]"} {
		if !strings.Contains(html, safe) {
			t.Fatal("missing supported rendering", safe, html)
		}
	}
	markdown := Markdown(body, ResourceAliases(resources, map[string]string{resources[0]: "assets/saved.png"}))
	for _, part := range []string{"**bold**", "> quoted", "````", "![img](assets/saved.png)", "<summary>details</summary>", "&lt;script&gt;"} {
		if !strings.Contains(markdown, part) {
			t.Fatal("missing Markdown content", part, markdown)
		}
	}
	if Text("[b]plain[/b]") != "plain" {
		t.Fatal("notification summary did not share markup parsing")
	}
}
