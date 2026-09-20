package content

import (
	"encoding/json"
	"os"
	"reflect"
	"strings"
	"testing"
)

func TestMarkdownNGAFixtures(t *testing.T) {
	raw, err := os.ReadFile("testdata/nga_markdown.json")
	if err != nil {
		t.Fatal(err)
	}
	var fixture struct {
		Cases []struct {
			Name, Body       string
			Contains, Absent []string
		}
	}
	if err := json.Unmarshal(raw, &fixture); err != nil {
		t.Fatal(err)
	}
	for _, test := range fixture.Cases {
		t.Run(test.Name, func(t *testing.T) {
			out := ParseMarkdown(test.Body, 1001).Render(nil, nil)
			for _, part := range test.Contains {
				if !strings.Contains(out, part) {
					t.Errorf("missing %q in:\n%s", part, out)
				}
			}
			for _, part := range test.Absent {
				if strings.Contains(out, part) {
					t.Errorf("unexpected %q in:\n%s", part, out)
				}
			}
		})
	}
}

func TestMarkdownReferencesAndCodeIsolation(t *testing.T) {
	body := `[quote][tid=1001]Topic[/tid] <b>Post by [uid=2001]writer[/uid] (2026-09-17 17:21):</b>outer
[quote][pid=4002,1002,1]Reply[/pid] <b>Post by 乙杨卓子罗麻<span class="gray">(2楼)</span> (2026-09-17 17:22):</b>inner[/quote]tail[/quote]
<b>Reply to [pid=5001,1001,1]Reply[/pid] Post by [uid=2002]reader[/uid] (2026-09-17 17:23)</b>reply
[code][s:ac:哭笑][b]literal[/b]<br/>\u003c [quote][tid=9000]Topic[/tid] <b>Post by nobody (2026-09-17 17:21):</b>x[/quote]
` + "```" + `[/code]`
	parsed := ParseMarkdown(body, 1001)
	want := []PostReference{{1001, 0}, {1002, 4002}, {1001, 5001}}
	if !reflect.DeepEqual(parsed.References, want) {
		t.Fatalf("references = %#v, want %#v", parsed.References, want)
	}
	out := parsed.Render(nil, map[PostReference]string{{1001, 0}: "#tid1001-pid0", {1001, 5001}: "#tid1001-pid5001"})
	for _, part := range []string{
		"[jump](#tid1001-pid0) writer(2001)(2026-09-17 17:21) 说: outer",
		"> > [jump](https://bbs.nga.cn/read.php?tid=1002&pid=4002)",
		"乙杨卓子罗麻(2楼)(2026-09-17 17:22) 说: inner",
		"[jump](#tid1001-pid5001)", "tail", "reply",
		"````\n[s:ac:哭笑][b]literal[/b]<br/>\\u003c [quote]", "\n```\n````",
	} {
		if !strings.Contains(out, part) {
			t.Errorf("missing %q in:\n%s", part, out)
		}
	}
}

func TestMarkdownImageAliases(t *testing.T) {
	image := "https://img.nga.cn/attachments/path/photo.png.medium.jpg"
	urls := []string{image, "https://img.nga.cn/audio.mp3"}
	local := MarkdownResourceAliases(urls, map[string]string{image: "assets/photo.png", urls[1]: "assets/audio.mp3"})
	remote := MarkdownResourceAliases(urls, nil)
	for _, raw := range []string{image, "./path/photo.png.medium.jpg", "./path/photo.png"} {
		if got := Markdown("[img]"+raw+"[/img]", local); got != "![img](assets/photo.png)" {
			t.Fatalf("local image %q rendered as %q", raw, got)
		}
		if got := Markdown("[img]"+raw+"[/img]", remote); got != "![img](https://img.nga.cn/attachments/path/photo.png)" {
			t.Fatalf("remote image %q rendered as %q", raw, got)
		}
	}
	if got := Markdown(`[audio=loop]https://img.nga.cn/audio.mp3[/audio]`, local); got != "【音频：assets/audio.mp3】" {
		t.Fatal(got)
	}
	if got := Markdown(`[url=https://example.invalid/path\]link[/url]`, nil); got != "[link](https://example.invalid/path%5C)" {
		t.Fatal("URL escaped the Markdown destination delimiter", got)
	}
}

func TestAnonymousNames(t *testing.T) {
	for _, test := range []struct{ input, want string }{
		{"#anony_105bdda13fb29b421690bf0cdc262b0b", "乙杨卓子罗麻?"},
		{"乙杨卓子罗麻", "乙杨卓子罗麻"},
		{"#anony_short", "#anony_short"},
		{"#anony_105bdda13fb29b421690bf0cdc262b0z", "#anony_105bdda13fb29b421690bf0cdc262b0z"},
		{"#anony_105bdda13fb29b421690bf0cdc262b0ba", "#anony_105bdda13fb29b421690bf0cdc262b0ba"},
	} {
		if got := AnonymousName(test.input); got != test.want {
			t.Errorf("AnonymousName(%q) = %q, want %q", test.input, got, test.want)
		}
	}
}

func TestMarkdownLiteralLessThan(t *testing.T) {
	if got := Markdown("1 < 2 [b]still bold[/b]", nil); got != "1 &lt; 2 **still bold**" {
		t.Fatal("literal less-than sign consumed following BBCode", got)
	}
}
