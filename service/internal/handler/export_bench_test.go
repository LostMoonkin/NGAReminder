package handler

import (
	"archive/zip"
	"bytes"
	"context"
	"encoding/base64"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"ngareminder/service/internal/config"
	"ngareminder/service/internal/infrastructure"
	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/repository"
	"ngareminder/service/internal/service"
)

func TestExportPerformanceInvariants(t *testing.T) {
	cfg := testConfig(t)
	var logs bytes.Buffer
	router, store, m := openAppWithTransport(t, cfg, &logs, exportNoNetwork{t})
	ctx := context.Background()
	now := time.Now().UTC().Truncate(time.Second)

	assets := infrastructure.Assets{Path: cfg.AssetsPath}
	pngPath, err := assets.Save(ctx, bytes.Repeat([]byte{0x89, 0x50, 0x4e, 0x47}, 256), "image/png", "https://img.nga.cn/photo.png")
	if err != nil {
		t.Fatal(err)
	}
	txtPath, err := assets.Save(ctx, bytes.Repeat([]byte("plain-text-attachment\n"), 64), "text/plain", "https://img.nga.cn/note.txt")
	if err != nil {
		t.Fatal(err)
	}
	outside := filepath.Join(t.TempDir(), "secret.png")
	if err = os.WriteFile(outside, []byte("outside-secret"), 0600); err != nil {
		t.Fatal(err)
	}
	if err = os.Symlink(outside, filepath.Join(cfg.AssetsPath, "symlink.png")); err != nil {
		t.Fatal(err)
	}
	for _, item := range []repository.Resource{
		{URL: "https://img.nga.cn/photo.png", Path: pngPath, MIME: "image/png", Size: 1024},
		{URL: "https://img.nga.cn/note.txt", Path: txtPath, MIME: "text/plain", Size: 1408},
		{URL: "https://img.nga.cn/symlink.png", Path: "symlink.png", MIME: "image/png", Size: 14},
	} {
		if err = store.SaveResource(ctx, &item); err != nil {
			t.Fatal(err)
		}
	}

	const count = 1200
	posts := make([]repository.Post, 0, count)
	rawBlob := []byte(`{"fixture":"` + strings.Repeat("x", 512) + `"}`)
	for i := 0; i < count; i++ {
		p := repository.Post{
			TID:         1001,
			Key:         fmt.Sprintf("pid:%d", 10000+i),
			PID:         int64(10000 + i),
			Kind:        "reply",
			Floor:       int64(i),
			AuthorUID:   int64(2001 + i%5),
			Author:      fmt.Sprintf("user-%d", i%5),
			Body:        fmt.Sprintf("post-body-%04d", i),
			PublishedAt: &now,
			SourceURL:   fmt.Sprintf("https://bbs.nga.cn/read.php?tid=1001&pid=%d", 10000+i),
			RawPayload:  rawBlob,
		}
		switch {
		case i == 0:
			p.Key, p.Kind, p.Floor, p.Subject = "main", "main", 0, "千楼性能回归主题"
			p.Resources = []string{"https://img.nga.cn/photo.png", "https://img.nga.cn/note.txt", "https://img.nga.cn/symlink.png"}
		case i%10 == 0:
			p.Kind, p.Floor, p.ParentFloor, p.ParentKey = "comment", 0, int64(i-5), fmt.Sprintf("pid:%d", 10000+i-5)
			p.Body = fmt.Sprintf("[quote][pid=%d,1001,1]Reply[/pid] <b>Post by [uid=2001]u[/uid] (2026-09-20 09:00):</b>earlier[/quote] comment-%04d", 10000+i-5, i)
		case i%3 == 0:
			targetPID := 10000 + i/2
			if i%9 == 0 {
				targetPID = 9999999 // 缺失引用
			}
			p.Body = fmt.Sprintf("[quote][pid=%d,1001,1]Reply[/pid] <b>Post by [uid=2001]u[/uid] (2026-09-20 09:00):</b>q[/quote] reply-%04d", targetPID, i)
			p.Resources = []string{"https://img.nga.cn/photo.png"}
		}
		posts = append(posts, p)
	}
	if _, err = store.InsertPosts(ctx, posts); err != nil {
		t.Fatal(err)
	}

	// 与原始 ContentBatch 排序结果严格核对顺序。
	ordered, err := store.ContentBatch(ctx, repository.ContentFilter{TID: 1001}, 0, count+10, int64(count+10))
	if err != nil || len(ordered) != count {
		t.Fatalf("ContentBatch = %d, %v", len(ordered), err)
	}

	logs.Reset()
	resp := send(t, router, "GET", "/api/v1/exports/threads/1001?format=zip", cfg.APIToken, nil)
	expectStatus(t, resp, 200)

	// 验证不再产生逐条引用查询或 OFFSET 批查。
	logText := logs.String()
	if strings.Contains(logText, "repository.content_reference_exists") {
		t.Fatal("export still executed per-reference SQL queries")
	}
	if strings.Contains(logText, "repository.content_batch") {
		t.Fatal("export still executed OFFSET ContentBatch queries")
	}
	if strings.Contains(logText, `"operation":"repository.resource"`) {
		t.Fatal("ZIP export still executed per-resource SQL queries")
	}
	if got := strings.Count(logText, `"operation":"repository.export_posts_by_ids"`); got != 9 {
		// 每个批查包含 start、sql、end 三条日志，1200 条按 500 分 3 批 => 9 条。
		t.Fatalf("expected 3 primary-key batches (9 log entries) for 1200 posts, got %d", got)
	}

	archive, err := zip.NewReader(bytes.NewReader(resp.Body.Bytes()), int64(resp.Body.Len()))
	if err != nil {
		t.Fatal(err)
	}
	methods := map[string]uint16{}
	var contentMD string
	for _, entry := range archive.File {
		methods[entry.Name] = entry.Method
		if entry.Name == "content.md" {
			r, openErr := entry.Open()
			if openErr != nil {
				t.Fatal(openErr)
			}
			raw, readErr := io.ReadAll(r)
			_ = r.Close()
			if readErr != nil {
				t.Fatal(readErr)
			}
			contentMD = string(raw)
		}
	}
	if methods["content.md"] != zip.Deflate || methods["metadata.json"] != zip.Deflate {
		t.Fatalf("text entries should use Deflate, got %+v", methods)
	}
	if methods["assets/"+pngPath] != zip.Store {
		t.Fatalf("PNG asset should use Store, got %d", methods["assets/"+pngPath])
	}
	if methods["assets/"+txtPath] != zip.Deflate {
		t.Fatalf("text asset should use Deflate, got %d", methods["assets/"+txtPath])
	}
	if _, leaked := methods["assets/symlink.png"]; leaked {
		t.Fatal("symlink asset was packed into ZIP")
	}
	if !strings.Contains(contentMD, "https://img.nga.cn/symlink.png") {
		t.Fatal("unavailable symlink resource did not fall back to remote URL")
	}

	// 验证所有 1200 条帖子的输出顺序与 ContentBatch 一致。
	lastPos := -1
	for _, post := range ordered {
		marker := fmt.Sprintf(`<a id="tid1001-pid%d"></a>`, post.PID)
		pos := strings.Index(contentMD, marker)
		if pos <= lastPos {
			t.Fatalf("post PID %d out of order (pos=%d, last=%d)", post.PID, pos, lastPos)
		}
		lastPos = pos
	}
	if !strings.Contains(contentMD, "[jump](#tid1001-pid10005)") || !strings.Contains(contentMD, "[jump](https://bbs.nga.cn/read.php?tid=1001&pid=9999999)") {
		t.Fatal("reference resolution mismatch in optimized export")
	}

	// 导出期间健康检查可用。
	expectStatus(t, request(t, router, "GET", "/readyz", ""), 200)
	_ = m
}

func BenchmarkLargeThreadExport(b *testing.B) {
	const totalPosts = 120000
	dir := b.TempDir()
	cfg := config.Config{
		ListenAddress: "127.0.0.1:0",
		DatabasePath:  filepath.Join(dir, "bench.db"),
		AssetsPath:    filepath.Join(dir, "assets"),
		APIToken:      "bench-token",
		EncryptionKey: base64.StdEncoding.EncodeToString(bytes.Repeat([]byte{7}, 32)),
		Timezone:      "Asia/Shanghai",
	}
	log := logging.New(io.Discard)
	ctx := log.WithContext(context.Background())
	if err := infrastructure.PrepareStorage(ctx, cfg.DatabasePath, cfg.AssetsPath); err != nil {
		b.Fatal(err)
	}
	store, err := repository.Open(ctx, cfg.DatabasePath)
	if err != nil {
		b.Fatal(err)
	}
	b.Cleanup(func() { _ = store.Close(ctx) })

	assets := infrastructure.Assets{Path: cfg.AssetsPath}
	imgData := bytes.Repeat([]byte{0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a}, 512)
	imgPath, err := assets.Save(ctx, imgData, "image/png", "https://img.nga.cn/bench.png")
	if err != nil {
		b.Fatal(err)
	}
	if err = store.SaveResource(ctx, &repository.Resource{URL: "https://img.nga.cn/bench.png", Path: imgPath, MIME: "image/png", Size: int64(len(imgData))}); err != nil {
		b.Fatal(err)
	}

	now := time.Now().UTC().Truncate(time.Second)
	const chunk = 2000
	for base := 0; base < totalPosts; base += chunk {
		batch := make([]repository.Post, 0, chunk)
		for j := 0; j < chunk && base+j < totalPosts; j++ {
			i := base + j
			p := repository.Post{
				TID:         47288722,
				Key:         fmt.Sprintf("pid:%d", 1000000+i),
				PID:         int64(1000000 + i),
				Kind:        "reply",
				Floor:       int64(i),
				AuthorUID:   int64(10000 + i%500),
				Author:      fmt.Sprintf("user-%d", i%500),
				PublishedAt: &now,
				SourceURL:   fmt.Sprintf("https://bbs.nga.cn/read.php?tid=47288722&pid=%d", 1000000+i),
			}
			switch {
			case i == 0:
				p.Key, p.Kind, p.Subject = "main", "main", "12万楼大型主题基准"
				p.Body = "主楼内容 [img]https://img.nga.cn/bench.png[/img]"
				p.Resources = []string{"https://img.nga.cn/bench.png"}
			case i%2 == 0:
				refPID := 1000000 + (i / 2)
				p.Body = fmt.Sprintf("[quote][pid=%d,47288722,1]Reply[/pid] <b>Post by [uid=10001]u[/uid] (2026-11-23 12:00):</b>引用内容[/quote] 第 %d 楼回复正文，包含一些讨论内容。", refPID, i)
			default:
				p.Body = fmt.Sprintf("第 %d 楼普通回复，[b]加粗[/b] 文本 [s:ac:哭笑]。", i)
			}
			if i%20 == 0 && i > 0 {
				p.Resources = []string{"https://img.nga.cn/bench.png"}
			}
			batch = append(batch, p)
		}
		if _, err = store.InsertPosts(ctx, batch); err != nil {
			b.Fatal(err)
		}
	}

	monitor, err := service.NewMonitoring(ctx, cfg, store, infrastructure.NewNGA(cfg.NGAUserAgent, nil), log, infrastructure.NewNotifier(nil))
	if err != nil {
		b.Fatal(err)
	}
	b.Cleanup(monitor.Close)

	for _, format := range []string{"markdown", "zip"} {
		b.Run(format, func(b *testing.B) {
			b.ResetTimer()
			for n := 0; n < b.N; n++ {
				start := time.Now()
				file, expErr := monitor.Export(ctx, "threads", 47288722, service.ExportInput{Format: format})
				if expErr != nil {
					b.Fatal(expErr)
				}
				info, statErr := file.Stat()
				if statErr != nil {
					b.Fatal(statErr)
				}
				elapsed := time.Since(start)
				if rmErr := monitor.RemoveExport(ctx, file); rmErr != nil {
					b.Fatal(rmErr)
				}
				b.ReportMetric(elapsed.Seconds(), "sec/op")
				b.ReportMetric(float64(totalPosts)/elapsed.Seconds(), "posts/sec")
				b.ReportMetric(float64(info.Size())/(1024*1024), "MiB/op")
			}
		})
	}
}
