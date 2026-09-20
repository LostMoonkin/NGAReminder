package handler

import (
	"archive/zip"
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"ngareminder/service/internal/repository"
)

type exportNoNetwork struct{ t *testing.T }

func (f exportNoNetwork) RoundTrip(r *http.Request) (*http.Response, error) {
	f.t.Errorf("export attempted a network request: %s", r.URL.Path)
	return nil, fmt.Errorf("network disabled in export fixture")
}

func TestMarkdownExportReferencesAndSavedMedia(t *testing.T) {
	cfg := testConfig(t)
	var logs bytes.Buffer
	router, store, _ := openAppWithTransport(t, cfg, &logs, exportNoNetwork{t})
	ctx := context.Background()
	when := time.Date(2026, 9, 17, 9, 21, 0, 0, time.UTC)
	posts := []repository.Post{}
	for i := 0; i < 206; i++ {
		p := repository.Post{TID: 1001, Key: fmt.Sprintf("pid:%d", 4000+i), PID: int64(4000 + i), Kind: "reply", Floor: int64(i), AuthorUID: 2001, Author: "writer", Body: fmt.Sprintf("entry-%03d", i), PublishedAt: &when}
		if i == 0 {
			p.Key, p.PID, p.Kind, p.Subject = "main", 0, "main", "First thread"
		}
		posts = append(posts, p)
	}
	posts[0].Body = `[quote][pid=4205,1001,11]Reply[/pid] <b>Post by [uid=2001]writer[/uid] (2026-09-17 17:21):</b>future batch[/quote]
[quote][tid=1002]Topic[/tid] <b>Post by [uid=2001]writer[/uid] (2026-09-17 17:21):</b>other main[/quote]
<b>Reply to [pid=5001,1001,1]Reply[/pid] Post by [uid=2001]writer[/uid] (2026-09-17 17:21)</b>comment
<b>Reply to [pid=9001,1001,1]Reply[/pid] Post by [uid=9000]other[/uid] (2026-09-17 17:21)</b>excluded author
<b>Reply to [pid=9999,1001,1]Reply[/pid] Post by [uid=9000]other[/uid] (2026-09-17 17:21)</b>missing target
[s:ac:哭笑][collapse=details]saved content[/collapse]
[img]./path/photo.png[/img][img]./path/photo.png.medium.jpg[/img]
<span class="audio" onclick="audioClick(event)"> <audio src="https://img.nga.cn/audio.mp3" controls></audio></span>
<span class="video"><video src="https://img.nga.cn/video.mp4"></video></span>
[img]https://img.nga.cn/missing.png[/img]`
	posts = append(posts,
		repository.Post{TID: 1001, Key: "pid:5001", PID: 5001, Kind: "comment", ParentKey: "main", ParentFloor: 0, AuthorUID: 2001, Author: "writer", Body: "comment-body"},
		repository.Post{TID: 1001, Key: "pid:9001", PID: 9001, Kind: "reply", Floor: 207, AuthorUID: 9000, Author: "other", Body: "excluded-body"},
		repository.Post{TID: 1001, Key: "pid:9002", PID: 9002, Kind: "reply", Floor: 208, AuthorUID: -1, Author: "乙杨卓子罗麻", Body: "readable-anonymous"},
		repository.Post{TID: 1001, Key: "pid:9003", PID: 9003, Kind: "reply", Floor: 209, AuthorUID: -1, Author: "#anony_105bdda13fb29b421690bf0cdc262b0b", Body: "encoded-anonymous"},
		repository.Post{TID: 1002, Key: "main", Kind: "main", AuthorUID: 2001, Author: "writer", Subject: "Second thread", Body: `[quote][tid=1001]Topic[/tid] <b>Post by [uid=2001]writer[/uid] (2026-09-17 17:21):</b>previous main[/quote]`},
	)
	resources := []repository.Resource{
		{URL: "https://img.nga.cn/attachments/path/photo.png.medium.jpg", Path: "photo.png"},
		{URL: "https://img.nga.cn/audio.mp3", Path: "audio.mp3"},
		{URL: "https://img.nga.cn/video.mp4", Path: "video.mp4"},
		{URL: "https://img.nga.cn/archive.7z", Path: "archive.7z"},
		{URL: "https://img.nga.cn/missing.png", Path: "missing.png"},
	}
	for _, item := range resources {
		posts[0].Resources = append(posts[0].Resources, item.URL)
		if err := store.SaveResource(ctx, &item); err != nil {
			t.Fatal(err)
		}
		if item.Path != "missing.png" {
			if err := os.WriteFile(filepath.Join(cfg.AssetsPath, item.Path), []byte("saved-"+item.Path), 0600); err != nil {
				t.Fatal(err)
			}
		}
	}
	posts[1].Resources = posts[0].Resources[:1]
	if _, err := store.InsertPosts(ctx, posts); err != nil {
		t.Fatal(err)
	}
	before, err := store.ContentBatch(ctx, repository.ContentFilter{}, 0, 1000, 1000)
	if err != nil {
		t.Fatal(err)
	}
	beforeJSON, _ := json.Marshal(before)
	getExport := func(path string) []byte {
		t.Helper()
		response := send(t, router, "GET", path, cfg.APIToken, nil)
		expectStatus(t, response, 200)
		return response.Body.Bytes()
	}
	md := getExport("/api/v1/exports/users/2001?format=markdown")
	for _, part := range []string{
		`id="tid1001-pid0"`, `id="tid1002-pid0"`, `id="tid1001-pid5001"`,
		"[jump](#tid1001-pid4205)", "[jump](#tid1002-pid0)", "[jump](#tid1001-pid0)", "[jump](#tid1001-pid5001)",
		"[jump](https://bbs.nga.cn/read.php?tid=1001&pid=9001)", "[jump](https://bbs.nga.cn/read.php?tid=1001&pid=9999)",
		"entry-205", "comment-body", "<summary>details</summary>", "![哭笑](https://img4.nga.178.com/ngabbs/post/smile/ac15.png)",
		"![img](https://img.nga.cn/attachments/path/photo.png)", "【音频：https://img.nga.cn/audio.mp3】", "【视频：https://img.nga.cn/video.mp4】",
	} {
		if !bytes.Contains(md, []byte(part)) {
			t.Errorf("missing %q in UID Markdown", part)
		}
	}
	if strings.Count(string(md), "## ") != 208 || bytes.Contains(md, []byte("excluded-body")) || bytes.Contains(md, []byte("anonymous")) {
		t.Fatal("UID export changed its content range or lost posts")
	}
	if !bytes.Equal(md, getExport("/api/v1/exports/users/2001?format=markdown")) {
		t.Fatal("repeated Markdown export changed")
	}
	thread := string(getExport("/api/v1/exports/threads/1001?format=markdown"))
	if !strings.Contains(thread, "[jump](#tid1001-pid9001)") || strings.Contains(thread, "[jump](#tid1002-pid0)") || !strings.Contains(thread, "## 208 楼 · 乙杨卓子罗麻\n") || !strings.Contains(thread, "## 209 楼 · 乙杨卓子罗麻?\n") {
		t.Fatal("TID references or anonymous names were incorrect")
	}
	zipped := getExport("/api/v1/exports/users/2001?format=zip")
	archive, err := zip.NewReader(bytes.NewReader(zipped), int64(len(zipped)))
	if err != nil {
		t.Fatal(err)
	}
	entries := map[string][]byte{}
	for _, file := range archive.File {
		if _, exists := entries[file.Name]; exists {
			t.Fatal("duplicate ZIP entry", file.Name)
		}
		r, err := file.Open()
		if err != nil {
			t.Fatal(err)
		}
		entries[file.Name], err = io.ReadAll(r)
		closeErr := r.Close()
		if err != nil || closeErr != nil {
			t.Fatal(err, closeErr)
		}
	}
	if len(entries) != 6 || !bytes.Contains(entries["metadata.json"], []byte(`"post_count":208`)) {
		t.Fatal("ZIP did not preserve metadata and resource deduplication", len(entries))
	}
	expectedZIP := string(md)
	// 普通 Markdown 的图片正文使用原图地址，附件列表保留已保存的源地址。
	expectedZIP = strings.ReplaceAll(expectedZIP, "https://img.nga.cn/attachments/path/photo.png)", "assets/photo.png)")
	for _, item := range resources[:4] {
		expectedZIP = strings.ReplaceAll(expectedZIP, item.URL, "assets/"+item.Path)
		if string(entries["assets/"+item.Path]) != "saved-"+item.Path {
			t.Fatal("ZIP resource differs from saved file", item.Path)
		}
	}
	if string(entries["content.md"]) != expectedZIP {
		t.Fatal("ZIP Markdown differs beyond resource paths")
	}
	after, err := store.ContentBatch(ctx, repository.ContentFilter{}, 0, 1000, 1000)
	if err != nil {
		t.Fatal(err)
	}
	afterJSON, _ := json.Marshal(after)
	if !bytes.Equal(beforeJSON, afterJSON) {
		t.Fatal("export mutated stored posts or anonymous names")
	}
	storedResources, err := store.Resources(ctx)
	if err != nil {
		t.Fatal(err)
	}
	for _, item := range storedResources {
		if item.Error != "" {
			t.Fatal("export changed resource state")
		}
	}
	files, err := os.ReadDir(cfg.AssetsPath)
	if err != nil || len(files) != 4 {
		t.Fatal("export downloaded files or left temporary output", len(files), err)
	}
}
