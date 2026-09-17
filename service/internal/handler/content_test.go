package handler

import (
	"archive/zip"
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"ngareminder/service/internal/infrastructure"
	"ngareminder/service/internal/repository"
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
	if _, err = m.Export(cancelled, "users", 2001, "zip"); err == nil {
		t.Fatal("cancelled export succeeded")
	}
	file, err := m.Export(ctx, "users", 2001, "zip")
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
