package main

import (
	"archive/zip"
	"bytes"
	"context"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/libtnb/sqlite"
	"gorm.io/gorm"
	"gorm.io/gorm/logger"
	"ngareminder/service/internal/config"
	"ngareminder/service/internal/handler"
	"ngareminder/service/internal/infrastructure"
	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/repository"
	"ngareminder/service/internal/service"
)

func readReport(t *testing.T, path string) report {
	t.Helper()
	raw, err := os.ReadFile(path + ".report.json")
	if err != nil {
		t.Fatal(err)
	}
	var r report
	if err = json.Unmarshal(raw, &r); err != nil {
		t.Fatal(err)
	}
	if !r.Ready {
		t.Fatal("report is not ready")
	}
	return r
}
func inspectDB(t *testing.T, path string) *gorm.DB {
	t.Helper()
	db, err := gorm.Open(sqlite.Open(path), &gorm.Config{Logger: logger.Discard})
	if err != nil {
		t.Fatal(err)
	}
	pool, err := db.DB()
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = pool.Close() })
	return db
}
func queryCount(t *testing.T, db *gorm.DB, sql string, args ...any) int64 {
	t.Helper()
	var n int64
	if err := db.Raw(sql, args...).Scan(&n).Error; err != nil {
		t.Fatal(err)
	}
	return n
}

func TestPostgreSQLMigration(t *testing.T) {
	conn, schema, cutover := preparePG(t)
	var logs bytes.Buffer
	log := logging.New(&logs)
	log.SetSecrets(os.Getenv("NGA_MIGRATE_PG_URL"), fixtureOldKey, fixtureNewKey)
	ctx := log.WithContext(context.Background())
	dir := t.TempDir()
	source := filepath.Join(dir, "source.pg.jsonl")
	if err := exportPG(ctx, schema, source); err != nil {
		t.Fatal(err)
	}
	// 导出完整列值，不经过 float64；未知表和旧诊断表也必须留存。
	raw, err := os.ReadFile(source)
	if err != nil {
		t.Fatal(err)
	}
	for _, expected := range []string{"9007199254740993", "中文完整快照", "notification_deliveries"} {
		if !bytes.Contains(raw, []byte(expected)) {
			t.Fatalf("snapshot omitted %s", expected)
		}
	}
	o := options{Output: filepath.Join(dir, "first.db"), Source: source, Timezone: "Asia/Shanghai"}
	if err = convert(ctx, o); err != nil {
		t.Fatal(err)
	}
	r := readReport(t, o.Output)
	if r.Source["posts"] != 4 || r.Target["posts"] != 4 || r.Target["watches"] != 4 || r.Source["legacy_extra"] != 1 || r.Converted["credentials_verified"] != 9 {
		t.Fatalf("unexpected report counts: %+v", r.Converted)
	}
	if r.Converted["checks_passed"] < 10 || r.SnapshotSHA256 == "" {
		t.Fatal("verification report is incomplete")
	}
	for _, path := range []string{source, o.Output, o.Output + ".report.json"} {
		info, e := os.Stat(path)
		if e != nil {
			t.Fatal(e)
		}
		if info.Mode().Perm() != 0600 {
			t.Fatal("migration artifacts must be private")
		}
	}
	for _, suffix := range []string{"-wal", "-shm"} {
		if _, e := os.Stat(o.Output + suffix); !errors.Is(e, os.ErrNotExist) {
			t.Fatal("output must be a standalone SQLite file", e)
		}
	}
	db := inspectDB(t, o.Output)
	for query, want := range map[string]int64{
		"SELECT COUNT(*) FROM posts WHERE pid=9007199254740993 AND author_uid=9007199254740993":                                 1,
		"SELECT COUNT(*) FROM posts WHERE kind='comment' AND parent_key='pid:4001' AND parent_floor=1 AND comment_to_id='4001'": 1,
		"SELECT COUNT(*) FROM inbox_events WHERE read=1":                                                                        1,
		"SELECT COUNT(*) FROM deliveries WHERE status='sent' AND attempts=2":                                                    1,
		"SELECT COUNT(*) FROM deliveries WHERE status='failed' AND attempts=1":                                                  1,
		"SELECT COUNT(*) FROM deliveries WHERE status IN ('pending','sending')":                                                 0,
		"SELECT COUNT(*) FROM runs WHERE status='interrupted'":                                                                  1,
		"SELECT COUNT(*) FROM backfills WHERE status='interrupted'":                                                             1,
		"SELECT COUNT(*) FROM renewal_requests WHERE status='interrupted' AND expected IS NULL":                                 1,
		"SELECT COUNT(*) FROM bot_settings WHERE code_hash='' AND code_expires IS NULL":                                         1,
		"SELECT COUNT(*) FROM floor_gaps WHERE status='expired'":                                                                1,
		"SELECT COUNT(*) FROM sqlite_master WHERE name='migration_source'":                                                      0,
	} {
		if n := queryCount(t, db, query); n != want {
			t.Fatalf("verification %s: got %d want %d", query, n, want)
		}
	}
	var watch repository.Watch
	if err = db.First(&watch, r.IDs["watch_targets"]["tid"]).Error; err != nil {
		t.Fatal(err)
	}
	if !watch.BaselineComplete || watch.CursorFloor != 3 || watch.HistoryBefore == nil || !watch.HistoryBefore.Equal(r.SnapshotAt) {
		t.Fatal("TID baseline or cutoff changed")
	}
	if !reflect.DeepEqual(watch.IntervalRules[0].Weekdays, []int{6, 7}) || watch.IntervalRules[0].End != "00:00" || watch.NoFetchPeriods[0].Weekdays[0] != 7 {
		t.Fatal("schedule weekday/end-of-day mapping failed")
	}
	watch = repository.Watch{}
	if err = db.First(&watch, r.IDs["watch_targets"]["uid"]).Error; err != nil {
		t.Fatal(err)
	}
	if watch.TopicCursor.ID != 1001 || watch.ReplyCursor.ID != 4001 || watch.ReplyCursor.Timestamp != 1767225660 {
		t.Fatal("UID watermark changed")
	}
	var gap repository.FloorGap
	if err = db.Where("watch_id=? AND floor=3", r.IDs["watch_targets"]["tid"]).First(&gap).Error; err != nil {
		t.Fatal(err)
	}
	if !gap.FirstSeen.Equal(cutover) || !gap.Deadline.Equal(cutover.Add(120*time.Second)) {
		t.Fatal("floor gap deadline was extended")
	}
	// 使用当前 Go 入口解密、浏览、导出；原 assets 由部署继续挂载，工具没有文件参数。
	verifyGoRead(t, o.Output, r, dir)
	second := o
	second.Output = filepath.Join(dir, "second.db")
	if err = convert(ctx, second); err != nil {
		t.Fatal(err)
	}
	r2 := readReport(t, second.Output)
	if !reflect.DeepEqual(r, r2) {
		t.Fatal("same snapshot produced different migration reports")
	}
	db2 := inspectDB(t, second.Output)
	for _, table := range []string{"watches", "posts", "runs", "inbox_events", "event_watches", "deliveries", "watch_posts", "floor_gaps", "backfills", "resources", "renewal_requests", "bot_receipts"} {
		var a, b []map[string]any
		if e := db.Table(table).Order("rowid").Find(&a).Error; e != nil {
			t.Fatal(e)
		}
		if e := db2.Table(table).Order("rowid").Find(&b).Error; e != nil {
			t.Fatal(e)
		}
		if !reflect.DeepEqual(a, b) {
			t.Fatalf("non-deterministic business data in %s", table)
		}
	}
	if err = convert(ctx, o); err == nil {
		t.Fatal("existing output was overwritten")
	}
	for name, change := range map[string][2]string{
		"wrong-key":   {"NGA_MIGRATE_OLD_KEY", fixtureNewKey},
		"bad-new-key": {"NGA_REMINDER_ENCRYPTION_KEY", "invalid"},
	} {
		t.Run(name, func(t *testing.T) {
			t.Setenv("NGA_MIGRATE_OLD_KEY", fixtureOldKey)
			t.Setenv("NGA_REMINDER_ENCRYPTION_KEY", fixtureNewKey)
			t.Setenv(change[0], change[1])
			failed := o
			failed.Output = filepath.Join(t.TempDir(), "failed.db")
			if e := convert(ctx, failed); e == nil {
				t.Fatal("invalid key accepted")
			}
			assertNotPublished(t, failed.Output)
		})
	}
	appBlob, _ := json.Marshal(`\x` + hex.EncodeToString(encryptedFixture(t, `{"platform":"feishu","credentials":{"app_id":"fake-second-app","app_secret":"fake-second-secret"}}`, "")))
	for name, mutate := range map[string]func(*sourceRecord){
		"extra-account": func(*sourceRecord) {},
		"multiple-apps": func(rec *sourceRecord) {
			if rec.Table == "platform_integrations" {
				replaceField(rec, "platform", `"feishu"`)
				replaceField(rec, "credentials_encrypted", string(appBlob))
			}
		},
		"bad-count": func(rec *sourceRecord) {
			if rec.Complete {
				rec.Counts["posts"]++
			}
		},
		"bad-cookie": func(rec *sourceRecord) {
			if rec.Table == "nga_accounts" {
				replaceField(rec, "cookie_encrypted", `"\\x00"`)
			}
		},
		"lower-role": func(rec *sourceRecord) {
			if rec.Table == "bot_bindings" {
				replaceField(rec, "role", `"operator"`)
			}
		},
		"dangling-comment": func(rec *sourceRecord) {
			if rec.Table == "posts" && bytes.Contains(rec.Row, []byte(`"comment"`)) {
				replaceField(rec, "parent_post_id", `"absent"`)
			}
		},
		"future-schema": func(rec *sourceRecord) {
			if rec.Table == "_sqlx_migrations" {
				replaceField(rec, "version", "99")
			}
		},
	} {
		t.Run(name, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "bad.pg.jsonl")
			rewriteSnapshot(t, source, path, mutate, name == "extra-account")
			failed := o
			failed.Source, failed.Output = path, filepath.Join(t.TempDir(), "failed.db")
			if e := convert(ctx, failed); e == nil {
				t.Fatal("unsupported source was accepted")
			}
			assertNotPublished(t, failed.Output)
		})
	}
	t.Run("truncated-snapshot", func(t *testing.T) {
		failed := o
		failed.Source = filepath.Join(t.TempDir(), "short.pg.jsonl")
		failed.Output = filepath.Join(t.TempDir(), "failed.db")
		if e := os.WriteFile(failed.Source, raw[:len(raw)/2], 0600); e != nil {
			t.Fatal(e)
		}
		if e := convert(ctx, failed); e == nil {
			t.Fatal("truncated snapshot accepted")
		}
		assertNotPublished(t, failed.Output)
	})
	t.Run("cancelled-conversion", func(t *testing.T) {
		cancelled, cancel := context.WithCancel(ctx)
		cancel()
		failed := o
		failed.Output = filepath.Join(t.TempDir(), "failed.db")
		if e := convert(cancelled, failed); e == nil {
			t.Fatal("cancelled conversion succeeded")
		}
		assertNotPublished(t, failed.Output)
	})
	// 再次导出并比较所有行，证明迁移未修改 PG。
	unchanged := filepath.Join(dir, "after.pg.jsonl")
	if err = exportPG(ctx, schema, unchanged); err != nil {
		t.Fatal(err)
	}
	after, err := os.ReadFile(unchanged)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(bytes.SplitN(raw, []byte("\n"), 2)[1], bytes.SplitN(after, []byte("\n"), 2)[1]) {
		t.Fatal("source PostgreSQL data changed")
	}
	if _, err = conn.Exec(ctx, "SELECT 1"); err != nil {
		t.Fatal(err)
	}
	verifyCLI(t, schema, source, dir)
	verifyIncrement(t, second.Output, r)
	for _, secret := range []string{fixtureOldKey, fixtureNewKey, "fake-cid-secret", "fake-app-secret", "fake-device-key", "fake-login-password", "fake-private-chat", "fake-recipient"} {
		if strings.Contains(logs.String(), secret) {
			t.Fatalf("migration logs exposed secret %s", secret)
		}
	}
}

func assertNotPublished(t *testing.T, path string) {
	t.Helper()
	for _, suffix := range []string{"", ".report.json"} {
		if _, err := os.Stat(path + suffix); !errors.Is(err, os.ErrNotExist) {
			t.Fatal("failed migration published output", err)
		}
	}
}
func replaceField(rec *sourceRecord, key, value string) {
	var fields map[string]json.RawMessage
	_ = json.Unmarshal(rec.Row, &fields)
	fields[key] = json.RawMessage(value)
	rec.Row, _ = json.Marshal(fields)
}
func rewriteSnapshot(t *testing.T, source, dest string, mutate func(*sourceRecord), extra bool) {
	t.Helper()
	in, err := os.Open(source)
	if err != nil {
		t.Fatal(err)
	}
	defer in.Close()
	out, err := os.Create(dest)
	if err != nil {
		t.Fatal(err)
	}
	defer out.Close()
	d, e := json.NewDecoder(in), json.NewEncoder(out)
	for {
		var rec sourceRecord
		err = d.Decode(&rec)
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			t.Fatal(err)
		}
		mutate(&rec)
		if rec.Complete && extra {
			rec.Counts["nga_accounts"]++
		}
		if err = e.Encode(rec); err != nil {
			t.Fatal(err)
		}
		if extra && rec.Table == "nga_accounts" {
			replaceField(&rec, "id", `"second-account"`)
			replaceField(&rec, "label", `"second"`)
			if err = e.Encode(rec); err != nil {
				t.Fatal(err)
			}
		}
	}
}

type offlineTransport struct{}

func (offlineTransport) RoundTrip(*http.Request) (*http.Response, error) {
	return nil, fmt.Errorf("unexpected external request in migration verification")
}

func verifyGoRead(t *testing.T, path string, r report, dir string) {
	t.Helper()
	ctx := context.Background()
	log := logging.New(io.Discard)
	assets := filepath.Join(dir, "original-assets")
	nested := "ab/" + strings.Repeat("a", 64) + ".png"
	if err := os.MkdirAll(filepath.Join(assets, "ab"), 0700); err != nil {
		t.Fatal(err)
	}
	data := []byte("fixture-file")
	assetPath := filepath.Join(assets, nested)
	if err := os.WriteFile(assetPath, data, 0600); err != nil {
		t.Fatal(err)
	}
	before, err := os.Stat(assetPath)
	if err != nil {
		t.Fatal(err)
	}
	cfg := config.Config{DatabasePath: path, AssetsPath: assets, Timezone: "Asia/Shanghai", EncryptionKey: fixtureNewKey, APIToken: "fixture-token"}
	store, err := repository.Open(ctx, path)
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close(ctx)
	monitor, err := service.NewMonitoring(ctx, cfg, store, infrastructure.NewNGA("fixture", offlineTransport{}), log, infrastructure.NewNotifier(offlineTransport{}))
	if err != nil {
		t.Fatal(err)
	}
	defer monitor.Close()
	admin, err := service.NewAdmin(ctx, cfg, store)
	if err != nil {
		t.Fatal(err)
	}
	router, err := handler.New(admin, monitor, log)
	if err != nil {
		t.Fatal(err)
	}
	account, err := store.Account(ctx)
	if err != nil {
		t.Fatal(err)
	}
	cipher, err := infrastructure.NewCredentialCipher(fixtureNewKey)
	if err != nil {
		t.Fatal(err)
	}
	creds, err := cipher.Decrypt(ctx, account.Cookie)
	if err != nil || creds.UID != "2001" || !strings.Contains(creds.Cookie, "fake-extra-cookie") {
		t.Fatal("Go could not read migrated Cookie", err)
	}
	app, err := monitor.Notifications().AppCredentials(ctx)
	if err != nil || app.AppID != "fake-app-id" || app.AppSecret != "fake-app-secret" {
		t.Fatal("Feishu credentials changed", err)
	}
	settings, err := store.RenewalSettings(ctx)
	if err != nil || !settings.Enabled {
		t.Fatal("renewal settings changed", err)
	}
	plain, err := cipher.Open(ctx, "renewal-secret-v1", settings.Secret)
	if err != nil || !bytes.Contains(plain, []byte("fake-login-password")) {
		t.Fatal("renewal password could not decrypt", err)
	}
	address, err := monitor.Bot().BindingAddress(ctx, r.IDs["bot_bindings"]["owner"])
	if err != nil || address.ChatID != "fake-private-chat" {
		t.Fatal("bot address changed", err)
	}
	channels, err := store.Channels(ctx)
	if err != nil {
		t.Fatal(err)
	}
	for _, c := range channels {
		plain, err = cipher.Open(ctx, "notification-target-v1", c.Secret)
		if err != nil || len(plain) == 0 {
			t.Fatal("notification target could not decrypt", err)
		}
	}
	for _, url := range []string{"/admin", "/api/v1/watches", "/admin/threads/1001", "/api/v1/exports/threads/1001?format=markdown", "/admin/assets/" + nested, "/api/v1/assets/" + nested} {
		req := httptest.NewRequest("GET", url, nil)
		req.Header.Set("Authorization", "Bearer "+cfg.APIToken)
		res := httptest.NewRecorder()
		router.ServeHTTP(res, req)
		if res.Code != 200 {
			t.Fatalf("Go verification %s returned %d: %s", url, res.Code, res.Body.String())
		}
		if strings.Contains(url, "assets/") && !bytes.Equal(res.Body.Bytes(), data) {
			t.Fatal("nested asset changed")
		}
		if strings.Contains(url, "markdown") && !strings.Contains(res.Body.String(), "完整中文正文") {
			t.Fatal("Chinese post body was lost")
		}
	}
	file, err := monitor.Export(ctx, "threads", 1001, "zip")
	if err != nil {
		t.Fatal(err)
	}
	info, err := file.Stat()
	if err != nil {
		t.Fatal(err)
	}
	zipFile, err := zip.NewReader(file, info.Size())
	if err != nil {
		t.Fatal(err)
	}
	found := false
	for _, entry := range zipFile.File {
		if entry.Name == "assets/"+nested {
			found = true
			reader, e := entry.Open()
			if e != nil {
				t.Fatal(e)
			}
			raw, e := io.ReadAll(reader)
			_ = reader.Close()
			if e != nil || !bytes.Equal(raw, data) {
				t.Fatal("ZIP asset differs", e)
			}
		}
	}
	if !found {
		t.Fatal("ZIP omitted original nested asset")
	}
	if err = monitor.RemoveExport(ctx, file); err != nil {
		t.Fatal(err)
	}
	scan, err := monitor.Resources().Scan(ctx)
	if err != nil || len(scan.Missing) != 2 {
		t.Fatal("missing/remote assets were not reported", err, len(scan.Missing))
	}
	current, err := os.Stat(assetPath)
	if err != nil || !os.SameFile(before, current) || !before.ModTime().Equal(current.ModTime()) {
		t.Fatal("original asset file was modified", err)
	}
	fresh, err := store.ClaimBotMessage(ctx, hashID("fake-app-id:fake-message-id"))
	if err != nil || fresh {
		t.Fatal("old bot message could replay", err)
	}
}

func verifyCLI(t *testing.T, schema, source, dir string) {
	t.Helper()
	binary := filepath.Join(dir, "pg-migrate")
	cmd := exec.Command("go", "build", "-o", binary, ".")
	if raw, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("build CLI: %v %s", err, raw)
	}
	output := filepath.Join(dir, "cli.db")
	cmd = exec.Command(binary, "-schema", schema, "-output", output)
	raw, err := cmd.CombinedOutput()
	if err != nil {
		t.Fatalf("CLI migration failed: %v %s", err, raw)
	}
	readReport(t, output)
	cmd = exec.Command(binary, "-source", source, "-output", filepath.Join(dir, "cli-offline.db"))
	cmd.Env = append(os.Environ(), "NGA_MIGRATE_PG_URL=postgres://unreachable.invalid/no-network")
	if raw, err = cmd.CombinedOutput(); err != nil {
		t.Fatalf("offline CLI failed: %v %s", err, raw)
	}
	cmd = exec.Command(binary, "-source", source, "-output", filepath.Join(dir, "cli-bad.db"))
	cmd.Env = append(os.Environ(), "NGA_MIGRATE_OLD_KEY="+fixtureNewKey)
	raw, err = cmd.CombinedOutput()
	if err == nil || !bytes.Contains(raw, []byte(`"stack":[{`)) || !bytes.Contains(raw, []byte("decryption failed")) {
		t.Fatalf("CLI did not explain failure with stack: %v %s", err, raw)
	}
	for _, secret := range []string{fixtureOldKey, fixtureNewKey, "fake-cid-secret", "fake-app-secret", "fake-login-password"} {
		if bytes.Contains(raw, []byte(secret)) {
			t.Fatal("CLI error leaked a secret")
		}
	}
	assertNotPublished(t, filepath.Join(dir, "cli-bad.db"))
}

type incrementalTransport struct{ cutoff time.Time }

func (f incrementalTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	response := func(raw string) *http.Response {
		return &http.Response{StatusCode: 200, Header: make(http.Header), Body: io.NopCloser(strings.NewReader(raw))}
	}
	if req.URL.Path == "/thread.php" {
		if req.URL.Query().Get("searchpost") == "1" {
			return response(`{"code":0,"result":{"__ROWS":2,"__R__ROWS_PAGE":20,"__T":[{"__P":{"tid":1001,"pid":4005,"authorid":2002,"postdate":1767225660}},{"__P":{"tid":1001,"pid":4001,"authorid":2002,"postdate":1767225660}}]}}`), nil
		}
		return response(`{"code":0,"result":{"__ROWS":1,"__T__ROWS_PAGE":20,"__T":[{"tid":1001,"authorid":2002,"postdate":1767225600}]}}`), nil
	}
	if req.URL.Path != "/app_api.php" {
		return nil, fmt.Errorf("unexpected outbound request")
	}
	if err := req.ParseForm(); err != nil {
		return nil, err
	}
	if req.Form.Get("pid") == "4005" {
		return response(`{"code":0,"tsubject":"fixture","result":[{"tid":1001,"pid":4005,"lou":5,"postdatetimestamp":1767225660,"content":"new same-second UID reply","author":{"uid":2002,"username":"author"}}]}`), nil
	}
	raw := fmt.Sprintf(`{"code":0,"currentPage":1,"totalPage":1,"perPage":20,"vrows":4,"fid":3001,"tsubject":"fixture","tauthorid":2001,"result":[
	 {"tid":1001,"pid":0,"lou":0,"content":"topic","author":{"uid":2001,"username":"author"}},
	 {"tid":1001,"pid":4001,"lou":1,"content":"reply","author":{"uid":2002,"username":"author"},"comments":[
	 {"tid":1001,"pid":5001,"lou":0,"content":"existing comment","postdatetimestamp":1767225670,"author":{"uid":2003,"username":"author"}},
	 {"tid":1001,"pid":5002,"lou":0,"content":"old unseen comment","postdatetimestamp":1767225680,"author":{"uid":2004,"username":"author"}},
	 {"tid":1001,"pid":5003,"lou":0,"content":"new comment","postdatetimestamp":%d,"author":{"uid":2004,"username":"author"}}]},
	 {"tid":1001,"pid":4003,"lou":3,"content":"recovered gap","postdatetimestamp":1767225700,"author":{"uid":2002,"username":"author"}},
	 {"tid":1001,"pid":4004,"lou":4,"content":"new reply","postdatetimestamp":%d,"author":{"uid":2002,"username":"author"}}]}`, f.cutoff.Add(time.Second).Unix(), f.cutoff.Add(time.Second).Unix())
	return &http.Response{StatusCode: 200, Header: make(http.Header), Body: io.NopCloser(strings.NewReader(raw))}, nil
}
func verifyIncrement(t *testing.T, path string, r report) {
	t.Helper()
	ctx := context.Background()
	log := logging.New(io.Discard)
	store, err := repository.Open(ctx, path)
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close(ctx)
	cfg := config.Config{DatabasePath: path, AssetsPath: t.TempDir(), Timezone: "Asia/Shanghai", EncryptionKey: fixtureNewKey, BackgroundEnabled: true}
	monitor, err := service.NewMonitoring(ctx, cfg, store, infrastructure.NewNGA("fixture", incrementalTransport{r.SnapshotAt}), log, infrastructure.NewNotifier(offlineTransport{}))
	if err != nil {
		t.Fatal(err)
	}
	defer monitor.Close()
	id := r.IDs["watch_targets"]["tid"]
	for i := 0; i < 2; i++ {
		run, e := startFixtureRun(ctx, monitor, id)
		if e != nil {
			t.Fatal(e)
		}
		deadline := time.Now().Add(5 * time.Second)
		for run.Status == "running" && time.Now().Before(deadline) {
			time.Sleep(10 * time.Millisecond)
			run, e = monitor.Run(ctx, run.ID)
			if e != nil {
				t.Fatal(e)
			}
		}
		if run.Status != "success" || run.Silent {
			t.Fatalf("incremental run failed or rebuilt baseline: %+v", run)
		}
	}
	db := inspectDB(t, path)
	if n := queryCount(t, db, "SELECT COUNT(*) FROM posts"); n != 7 {
		t.Fatalf("expected only gap and new content, got %d", n)
	}
	if n := queryCount(t, db, "SELECT COUNT(*) FROM posts WHERE pid=5002"); n != 0 {
		t.Fatal("historical unseen comment was imported as new")
	}
	if n := queryCount(t, db, "SELECT COUNT(*) FROM inbox_events"); n != 5 {
		t.Fatalf("events were lost or duplicated: %d", n)
	}
	if n := queryCount(t, db, "SELECT COUNT(*) FROM deliveries WHERE status='sent' AND attempts=2"); n != 1 {
		t.Fatal("previously sent notification changed")
	}
	if n := queryCount(t, db, "SELECT COUNT(*) FROM watches WHERE id=? AND baseline_complete=1 AND cursor_floor=4", id); n != 1 {
		t.Fatal("cursor did not continue from original floor")
	}
	if n := queryCount(t, db, "SELECT COUNT(*) FROM floor_gaps WHERE watch_id=? AND floor=3 AND status='resolved'", id); n != 1 {
		t.Fatal("migrated gap below history cutoff was not recovered")
	}
	for i := 0; i < 2; i++ {
		run, e := startFixtureRun(ctx, monitor, r.IDs["watch_targets"]["uid"])
		if e != nil {
			t.Fatal(e)
		}
		deadline := time.Now().Add(5 * time.Second)
		for run.Status == "running" && time.Now().Before(deadline) {
			time.Sleep(10 * time.Millisecond)
			run, e = monitor.Run(ctx, run.ID)
			if e != nil {
				t.Fatal(e)
			}
		}
		if run.Status != "success" || run.Silent {
			t.Fatalf("UID incremental run failed or rebuilt baseline: %+v", run)
		}
	}
	watch, err := store.Watch(ctx, r.IDs["watch_targets"]["uid"])
	if err != nil {
		t.Fatal(err)
	}
	if watch.ReplyCursor.ID != 4005 || watch.ReplyCursor.Timestamp != 1767225660 || watch.TopicCursor.ID != 1001 {
		t.Fatal("UID migration lost the timestamp/ID watermark")
	}
	if n := queryCount(t, db, "SELECT COUNT(*) FROM posts"); n != 8 {
		t.Fatalf("UID incremental content duplicated: %d", n)
	}
	if n := queryCount(t, db, "SELECT COUNT(*) FROM inbox_events"); n != 6 {
		t.Fatalf("UID incremental events duplicated: %d", n)
	}
	newWatch := repository.Watch{Kind: "tid", TID: 9001, InitMode: "full", State: "ready", IntervalSeconds: 60}
	if err = store.SaveWatch(ctx, &newWatch); err != nil {
		t.Fatal(err)
	}
	if newWatch.ID <= int64(len(r.IDs["watch_targets"])) {
		t.Fatal("new watch reused a deleted historical watch ID")
	}
}

func startFixtureRun(ctx context.Context, monitor *service.Monitoring, id int64) (repository.Run, error) {
	deadline := time.Now().Add(5 * time.Second)
	for {
		run, err := monitor.StartRun(ctx, id)
		if !errors.Is(err, service.ErrBusy) || time.Now().After(deadline) {
			return run, err
		}
		time.Sleep(10 * time.Millisecond)
	}
}
