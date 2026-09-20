package handler

import (
	"archive/zip"
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"net/http/httptest"
	"net/url"
	"ngareminder/service/internal/infrastructure"
	"ngareminder/service/internal/repository"
	"ngareminder/service/internal/service"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestContentExportAndResourceMaintenance(t *testing.T) {
	cfg := testConfig(t)
	cfg.BackgroundEnabled = true
	var logs bytes.Buffer
	f := &noticeFixture{nga: fixtureNGA(t)}
	router, store, m := openAppWithTransport(t, cfg, &logs, f)
	ctx := context.Background()
	imageURL := "https://img.nga.cn/image.png"
	missingURL := "https://img.nga.cn/missing.png"
	now := time.Now().UTC()
	posts := []repository.Post{}
	for i := 0; i < 215; i++ {
		uid, tid := int64(2001), int64(1001)
		if i >= 205 {
			tid = 1002
		}
		if i == 214 {
			uid = 9000
		}
		posts = append(posts, repository.Post{TID: tid, Key: fmt.Sprintf("pid:%d", 4000+i), PID: int64(4000 + i), Kind: "reply", Floor: int64(i), AuthorUID: uid, Author: "fixture author", Body: fmt.Sprintf("[b]entry-%03d[/b] [quote]quoted[/quote][code]<script>fixture</script>[/code][img]%s[/img]", i, imageURL), PublishedAt: &now, SourceURL: fmt.Sprintf("https://bbs.nga.cn/read.php?tid=%d&pid=%d", tid, 4000+i), Resources: []string{imageURL, missingURL}})
	}
	if _, err := store.InsertPosts(ctx, posts); err != nil {
		t.Fatal(err)
	}
	// 下载关闭时仍能完整导出；读取不会访问 NGA。
	m.Resources().Collect(ctx, posts[:1])
	scan, err := m.Resources().Scan(ctx)
	if err != nil || len(scan.Missing) != 2 {
		t.Fatal("missing remote resources were not visible", err)
	}
	response := send(t, router, "GET", "/api/v1/users/2001/posts?page=2", cfg.APIToken, nil)
	expectStatus(t, response, 200)
	if !strings.Contains(response.Body.String(), "entry-050") || strings.Contains(response.Body.String(), "entry-214") {
		t.Fatal("UID pagination included wrong content")
	}
	response = send(t, router, "GET", "/admin/users/2001", "", nil)
	expectStatus(t, response, 200)
	if strings.Contains(response.Body.String(), "<script>fixture</script>") || !strings.Contains(response.Body.String(), "<blockquote>") {
		t.Fatal("unsafe or missing rich content")
	}
	response = send(t, router, "GET", "/api/v1/exports/users/2001?format=markdown", cfg.APIToken, nil)
	expectStatus(t, response, 200)
	first := response.Body.String()
	if strings.Count(first, "## ") != 214 || strings.Count(first, "# TID 1001") != 1 || strings.Count(first, "# TID 1002") != 1 || strings.Contains(first, "entry-214") {
		t.Fatal("multi-batch UID export was incomplete or not grouped")
	}
	response = send(t, router, "GET", "/api/v1/exports/users/2001?format=markdown", cfg.APIToken, nil)
	if response.Body.String() != first {
		t.Fatal("Markdown order was unstable")
	}
	expectStatus(t, send(t, router, "POST", "/api/v1/resources", cfg.APIToken, map[string]bool{"download_enabled": true}), 200)
	expectStatus(t, send(t, router, "POST", "/api/v1/resources/redownload", cfg.APIToken, nil), 200)
	resource, err := store.Resource(ctx, imageURL)
	if err != nil || resource.Path == "" {
		t.Fatal("supported image did not download", err)
	}
	response = send(t, router, "GET", "/api/v1/exports/users/2001?format=zip", cfg.APIToken, nil)
	expectStatus(t, response, 200)
	archive, err := zip.NewReader(bytes.NewReader(response.Body.Bytes()), int64(response.Body.Len()))
	if err != nil {
		t.Fatal(err)
	}
	if len(archive.File) != 3 {
		t.Fatal("ZIP did not deduplicate shared resources", len(archive.File))
	}
	for _, entry := range archive.File {
		if entry.Name == "content.md" {
			reader, _ := entry.Open()
			raw, _ := io.ReadAll(reader)
			_ = reader.Close()
			if !bytes.Contains(raw, []byte("assets/"+resource.Path)) || !bytes.Contains(raw, []byte(missingURL)) {
				t.Fatal("ZIP links did not point to packaged and remote resources")
			}
		}
	}
	files, err := os.ReadDir(cfg.AssetsPath)
	if err != nil {
		t.Fatal(err)
	}
	for _, entry := range files {
		if strings.HasPrefix(entry.Name(), ".export-") {
			t.Fatal("completed export left a temporary file")
		}
	}
	// 中断生成与中断发送均清理临时文件。
	cancelled, cancel := context.WithCancel(ctx)
	cancel()
	if _, err = m.Export(cancelled, "users", 2001, service.ExportInput{Format: "zip"}); err == nil {
		t.Fatal("cancelled export succeeded")
	}
	file, err := m.Export(ctx, "users", 2001, service.ExportInput{Format: "zip"})
	if err != nil {
		t.Fatal(err)
	}
	if err = m.SendExport(ctx, failingWriter{}, file); err == nil {
		t.Fatal("broken stream was accepted")
	}
	if err = m.RemoveExport(ctx, file); err != nil {
		t.Fatal(err)
	}
	// 老旧但仍有引用的文件保留；扫描及清理都不跟随符号链接。
	old := time.Now().Add(-48 * time.Hour)
	referenced := filepath.Join(cfg.AssetsPath, resource.Path)
	if err = os.Chtimes(referenced, old, old); err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"orphan.bin", ".export-abandoned.zip", "recent.bin"} {
		path := filepath.Join(cfg.AssetsPath, name)
		if err = os.WriteFile(path, []byte("fixture"), 0600); err != nil {
			t.Fatal(err)
		}
		if name != "recent.bin" {
			_ = os.Chtimes(path, old, old)
		}
	}
	outside := filepath.Join(t.TempDir(), "keep.bin")
	_ = os.WriteFile(outside, []byte("keep"), 0600)
	_ = os.Chtimes(outside, old, old)
	if err = os.Symlink(outside, filepath.Join(cfg.AssetsPath, "outside-link")); err != nil {
		t.Fatal(err)
	}
	scan, err = m.Resources().Scan(ctx)
	if err != nil || len(scan.Files) != 2 || len(scan.Temporary) != 1 {
		t.Fatal("read-only scan did not classify files", err, scan)
	}
	if _, err = os.Stat(filepath.Join(cfg.AssetsPath, "orphan.bin")); err != nil {
		t.Fatal("scan modified a file")
	}
	expectStatus(t, send(t, router, "POST", "/api/v1/resources/cleanup", cfg.APIToken, map[string]bool{"confirm": false}), 400)
	response = send(t, router, "POST", "/api/v1/resources/cleanup", cfg.APIToken, map[string]bool{"confirm": true})
	expectStatus(t, response, 200)
	for _, path := range []string{outside, referenced, filepath.Join(cfg.AssetsPath, "recent.bin"), filepath.Join(cfg.AssetsPath, "outside-link")} {
		if _, err = os.Lstat(path); err != nil {
			t.Fatal("cleanup removed a protected file", path, err)
		}
	}
	if _, err = os.Stat(filepath.Join(cfg.AssetsPath, "orphan.bin")); !errors.Is(err, os.ErrNotExist) {
		t.Fatal("old orphan was not removed")
	}
	// 删除被引用文件后，可重新下载；正文和其他资源仍保留。
	if err = os.Remove(referenced); err != nil {
		t.Fatal(err)
	}
	if _, err = m.Resources().Redownload(ctx); err != nil {
		t.Fatal(err)
	}
	if _, err = os.Stat(referenced); err != nil {
		t.Fatal("missing resource was not restored")
	}
	expectStatus(t, request(t, router, "GET", "/admin/resources", ""), 200)
	if !strings.Contains(logs.String(), "Resource download failed") && !strings.Contains(logs.String(), "Missing resource download failed") {
		t.Fatal("resource failures were not logged")
	}
}

func TestContentExportTimeRange(t *testing.T) {
	cfg := testConfig(t)
	router, store, _ := openAppWithTransport(t, cfg, io.Discard, exportNoNetwork{t})
	ctx := context.Background()
	at := func(value string) *time.Time {
		t.Helper()
		parsed, err := time.Parse(time.RFC3339, value)
		if err != nil {
			t.Fatal(err)
		}
		return &parsed
	}
	posts := []repository.Post{
		{TID: 1001, Key: "pid:1", PID: 1, Kind: "reply", Floor: 1, AuthorUID: 2001, Author: "writer", Body: "before-range", PublishedAt: at("2026-09-20T00:59:59Z")},
		{TID: 1001, Key: "pid:2", PID: 2, Kind: "reply", Floor: 2, AuthorUID: 2001, Author: "writer", Body: "start-boundary", PublishedAt: at("2026-09-20T01:00:00Z")},
		{TID: 1001, Key: "pid:3", PID: 3, Kind: "reply", Floor: 3, AuthorUID: 2001, Author: "writer", Body: "middle-range", PublishedAt: at("2026-09-20T01:00:01Z")},
		{TID: 1001, Key: "pid:4", PID: 4, Kind: "reply", Floor: 4, AuthorUID: 2001, Author: "writer", Body: "end-boundary", PublishedAt: at("2026-09-20T01:00:02Z")},
		{TID: 1001, Key: "pid:5", PID: 5, Kind: "reply", Floor: 5, AuthorUID: 2001, Author: "writer", Body: "after-range", PublishedAt: at("2026-09-20T01:00:03Z")},
		{TID: 1001, Key: "pid:6", PID: 6, Kind: "reply", Floor: 6, AuthorUID: 2001, Author: "writer", Body: "unknown-time"},
		{TID: 1001, Key: "pid:7", PID: 7, Kind: "reply", Floor: 7, AuthorUID: 9000, Author: "other", Body: "other-author", PublishedAt: at("2026-09-20T01:00:01Z")},
	}
	if _, err := store.InsertPosts(ctx, posts); err != nil {
		t.Fatal(err)
	}

	export := func(path string) *httptest.ResponseRecorder {
		t.Helper()
		response := send(t, router, "GET", path, cfg.APIToken, nil)
		expectStatus(t, response, 200)
		return response
	}
	query := url.Values{"format": {"markdown"}, "start_at": {"2026-09-20T01:00:00Z"}, "end_at": {"2026-09-20T01:00:02Z"}}
	uid := export("/api/v1/exports/users/2001?" + query.Encode()).Body.String()
	for _, included := range []string{"start-boundary", "middle-range", "end-boundary"} {
		if !strings.Contains(uid, included) {
			t.Error("closed time range omitted", included)
		}
	}
	for _, excluded := range []string{"before-range", "after-range", "unknown-time", "other-author"} {
		if strings.Contains(uid, excluded) {
			t.Error("UID time range included", excluded)
		}
	}

	localQuery := url.Values{"format": {"markdown"}, "start_at": {"2026-09-20T09:00:00"}, "end_at": {"2026-09-20T09:00:02"}}
	if local := export("/api/v1/exports/users/2001?" + localQuery.Encode()).Body.String(); local != uid {
		t.Fatal("configured timezone range differed from equivalent RFC3339 range")
	}
	minuteQuery := url.Values{"format": {"markdown"}, "start_at": {"2026-09-20T09:00"}, "end_at": {"2026-09-20T09:00"}}
	minute := export("/api/v1/exports/users/2001?" + minuteQuery.Encode()).Body.String()
	if !strings.Contains(minute, "start-boundary") || strings.Contains(minute, "middle-range") {
		t.Fatal("normalized datetime-local minute value did not mean second zero")
	}

	query.Set("format", "zip")
	response := export("/api/v1/exports/threads/1001?" + query.Encode())
	archive, err := zip.NewReader(bytes.NewReader(response.Body.Bytes()), int64(response.Body.Len()))
	if err != nil {
		t.Fatal(err)
	}
	var zipped string
	for _, entry := range archive.File {
		if entry.Name != "content.md" {
			continue
		}
		reader, openErr := entry.Open()
		if openErr != nil {
			t.Fatal(openErr)
		}
		raw, readErr := io.ReadAll(reader)
		closeErr := reader.Close()
		if readErr != nil || closeErr != nil {
			t.Fatal(readErr, closeErr)
		}
		zipped = string(raw)
	}
	if !strings.Contains(zipped, "other-author") || strings.Contains(zipped, "before-range") || strings.Contains(zipped, "unknown-time") {
		t.Fatal("TID ZIP did not apply the closed time range")
	}

	oneSided := url.Values{"format": {"markdown"}, "start_at": {"2026-09-20T01:00:02Z"}}
	result := export("/api/v1/exports/users/2001?" + oneSided.Encode()).Body.String()
	if !strings.Contains(result, "end-boundary") || !strings.Contains(result, "after-range") || strings.Contains(result, "middle-range") || strings.Contains(result, "unknown-time") {
		t.Fatal("one-sided start range was not inclusive")
	}
	full := export("/api/v1/exports/users/2001?format=markdown").Body.String()
	if !strings.Contains(full, "before-range") || !strings.Contains(full, "unknown-time") {
		t.Fatal("empty range parameters changed full export")
	}

	for _, path := range []string{
		"/api/v1/exports/users/2001?format=markdown&start_at=invalid",
		"/api/v1/exports/users/2001?format=markdown&start_at=2026-09-20T01%3A00%3A00.5Z",
		"/api/v1/exports/users/2001?format=markdown&start_at=2026-09-20T01%3A00%3A00.000Z",
		"/api/v1/exports/users/2001?format=markdown&start_at=2026-09-20T01%3A00%3A02Z&end_at=2026-09-20T01%3A00%3A00Z",
		"/api/v1/exports/users/2001?format=markdown&start_at=2099-09-20T01%3A00%3A00Z",
	} {
		expectStatus(t, send(t, router, "GET", path, cfg.APIToken, nil), 400)
	}
	files, err := os.ReadDir(cfg.AssetsPath)
	if err != nil {
		t.Fatal(err)
	}
	for _, entry := range files {
		if strings.HasPrefix(entry.Name(), ".export-") {
			t.Fatal("failed time range export left a temporary file")
		}
	}

	page := request(t, router, "GET", "/admin/threads/1001", "")
	expectStatus(t, page, 200)
	for _, marker := range []string{"data-open-export", "data-export-dialog", `type="datetime-local"`, "Asia/Shanghai", "首尾均包含", "no-js-only"} {
		if !strings.Contains(page.Body.String(), marker) {
			t.Error("export UI missing", marker)
		}
	}
	if strings.Contains(page.Body.String(), "下载 Markdown") || strings.Contains(page.Body.String(), "导出 ZIP") {
		t.Fatal("content page retained separate export buttons")
	}
}

type failingWriter struct{}

func (failingWriter) Write([]byte) (int, error) { return 0, errors.New("fixture disconnected client") }
func TestAssetRootRejectsSymlinkAndTraversal(t *testing.T) {
	root, outside := t.TempDir(), t.TempDir()
	files := infrastructure.Assets{Path: root}
	if err := os.WriteFile(filepath.Join(outside, "keep"), []byte("fixture"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(outside, filepath.Join(root, "directory-link")); err != nil {
		t.Fatal(err)
	}
	if _, err := files.Open(context.Background(), "../keep"); err == nil {
		t.Fatal("asset traversal accepted")
	}
	if removed, err := files.RemoveUnreferenced(context.Background(), "directory-link/keep", time.Now().Add(time.Hour)); err != nil || removed {
		t.Fatal("cleanup followed a directory symlink", err)
	}
	if _, err := os.Stat(filepath.Join(outside, "keep")); err != nil {
		t.Fatal("cleanup crossed the resource root")
	}
}
func TestCollectionRetainsContentOnResourceFailure(t *testing.T) {
	cfg := testConfig(t)
	cfg.BackgroundEnabled = true
	f := &noticeFixture{nga: fixtureNGA(t)}
	router, store, m := openAppWithTransport(t, cfg, io.Discard, f)
	f.nga.page1["result"].([]any)[0].(map[string]any)["content"] = "[img]https://img.nga.cn/missing.png[/img]"
	expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, map[string]string{"passport_uid": "2001", "passport_cid": "fixture-valid-secret"}), 200)
	if err := m.Resources().SaveSettings(context.Background(), true); err != nil {
		t.Fatal(err)
	}
	watch := createWatch(t, router, cfg.APIToken, 1001, "full")
	if run := runWatch(t, router, cfg.APIToken, watch.ID); run.Status != "success" || run.Saved == 0 {
		t.Fatal("resource failure interrupted content collection", run)
	}
	state, err := store.Watch(context.Background(), watch.ID)
	if err != nil || !state.BaselineComplete {
		t.Fatal("resource failure prevented baseline completion", err)
	}
}
