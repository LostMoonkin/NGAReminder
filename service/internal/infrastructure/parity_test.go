package infrastructure

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestRawPostAndPageMetadata(t *testing.T) {
	raw, err := os.ReadFile("testdata/nga/thread_comments_hot_post.json")
	if err != nil {
		t.Fatal(err)
	}
	page, err := parseThreadPage(raw, 1001, 1)
	if err != nil {
		t.Fatal(err)
	}
	if page.Metadata.FID != 3001 || page.Metadata.AuthorUID != 2001 || page.Metadata.ForumName == "" {
		t.Fatal("thread metadata missing", page.Metadata)
	}
	for _, p := range page.Posts {
		if p.PageNumber != 1 || p.Thread.TID != 1001 || !json.Valid(p.RawPayload) {
			t.Fatal("post provenance missing", p.Key)
		}
		var saved map[string]json.RawMessage
		if err = json.Unmarshal(p.RawPayload, &saved); err != nil {
			t.Fatal(err)
		}
		if _, ok := saved["currentPage"]; ok {
			t.Fatal("saved page envelope instead of post")
		}
		if p.Kind == "comment" && string(saved["type"]) != "0" {
			t.Fatal("unknown comment fields were discarded")
		}
	}
	replyRaw, _ := os.ReadFile("testdata/nga/post_by_pid_success.json")
	reply, err := parsePostByPID(replyRaw, 1001, 4001)
	if err != nil || reply.PageNumber != 1 || reply.Thread.Title == "" || !strings.Contains(string(reply.RawPayload), `"vote_good": 1`) {
		t.Fatal("reply metadata or raw missing", err)
	}
}

func TestResourceParity(t *testing.T) {
	ctx := context.Background()
	var calls int
	status, length, mime, payload := 206, int64(-1), "application/octet-stream", "7z-fixture"
	sender := NewNotifier(roundTrip(func(r *http.Request) (*http.Response, error) {
		calls++
		return &http.Response{StatusCode: status, Header: http.Header{"Content-Type": {mime}, "Location": {"https://untrusted.invalid/"}}, Body: io.NopCloser(strings.NewReader(payload)), ContentLength: length, Request: r}, nil
	}))
	defer sender.Close()
	assets := Assets{Path: t.TempDir()}
	for _, host := range []string{"img.nga.cn", "img.nga.178.com", "img4.nga.178.com", "img6.nga.cn", "img7.nga.cn", "img8.nga.cn"} {
		source := "https://" + host + "/download"
		data, typ, err := sender.DownloadResource(ctx, source, 32, false)
		if err != nil {
			t.Fatal(host, err)
		}
		name, err := assets.Save(ctx, data, typ, source, "archive.7z")
		if err != nil || filepath.Ext(name) != ".7z" {
			t.Fatal("7z original extension missing", name, err)
		}
	}
	for _, source := range []string{"http://img.nga.cn/file", "https://img6.nga.cn:443/file", "https://user@img7.nga.cn/file", "https://img8.nga.cn.invalid/file"} {
		before := calls
		if _, _, err := sender.DownloadResource(ctx, source, 32, false); err == nil || calls != before {
			t.Fatal("untrusted URL requested", source)
		}
	}
	before := calls
	if _, _, err := sender.DownloadResource(ctx, "https://img6.nga.cn/file.png", 32, true); err == nil || calls != before {
		t.Fatal("Feishu host policy was broadened")
	}
	for _, tc := range []struct{ mime, source, want string }{
		{"audio/mpeg", "https://img.nga.cn/", ".mp3"}, {"video/mp4", "https://img.nga.cn/", ".mp4"}, {"application/octet-stream", "https://img.nga.cn/", ".bin"},
		{"text/plain", "https://img.nga.cn/file.A-B_1234567890123", ".ab12345678"},
	} {
		name, err := assets.Save(ctx, []byte("content"), tc.mime, tc.source)
		if err != nil || filepath.Ext(name) != tc.want {
			t.Fatal("extension fallback mismatch", name, err)
		}
	}
	source := "https://img7.nga.cn/a"
	status = 302
	before = calls
	if _, _, err := sender.DownloadResource(ctx, source, 32, false); err == nil || calls != before+1 {
		t.Fatal("redirect was followed or accepted")
	}
	status, length, payload = 204, 0, ""
	if data, _, err := sender.DownloadResource(ctx, source, 32, false); err != nil || len(data) != 0 {
		t.Fatal("empty successful attachment rejected", err)
	}
	status, length = 200, 33
	if _, _, err := sender.DownloadResource(ctx, source, 32, false); err == nil {
		t.Fatal("oversized Content-Length accepted")
	}
	length, payload = -1, strings.Repeat("x", 33)
	if _, _, err := sender.DownloadResource(ctx, source, 32, false); err == nil {
		t.Fatal("oversized stream accepted")
	}
}
