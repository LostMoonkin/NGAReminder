package handler

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"strings"
	"sync"
	"testing"
	"time"

	"ngareminder/service/internal/repository"
	"ngareminder/service/internal/service"
)

// 所有请求仍经过真实 NGA client、Gin、service 和 SQLite；只替换远端 HTTP transport。
type ngaFixture struct {
	mu         sync.Mutex
	page1      map[string]any
	page2      map[string]any
	code       int
	failPage   int
	newPosts   bool
	block      <-chan struct{}
	calls      int
	seenCookie string
}

func readFixture(t *testing.T, name string) map[string]any {
	t.Helper()
	b, err := os.ReadFile("../infrastructure/testdata/nga/" + name + ".json")
	if err != nil {
		t.Fatal(err)
	}
	var result map[string]any
	if err = json.Unmarshal(b, &result); err != nil {
		t.Fatal(err)
	}
	return result
}

func fixtureNGA(t *testing.T) *ngaFixture {
	first := readFixture(t, "thread_comments_hot_post")
	first["totalPage"] = 2
	second := readFixture(t, "thread_page_success")
	second["currentPage"], second["totalPage"] = 2, 2
	p := second["result"].([]any)[1].(map[string]any)
	p["pid"], p["lou"], p["content"] = 4003, 7, "<script>fixture-only</script>[b]Floor 7[/b]"
	second["result"] = []any{p}
	return &ngaFixture{page1: first, page2: second}
}

func (f *ngaFixture) RoundTrip(r *http.Request) (*http.Response, error) {
	f.mu.Lock()
	f.calls++
	block := f.block
	f.mu.Unlock()
	if block != nil {
		select {
		case <-r.Context().Done():
			return nil, r.Context().Err()
		case <-block:
		}
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	if r.URL.Path == "/thread.php" {
		if !strings.Contains(r.Header.Get("Cookie"), "ngaPassportCid=fixture-valid-secret") {
			return fixtureResponse(`{"code":2048,"msg":"必须登录"}`), nil
		}
		return fixtureResponse(`{"code":0,"result":{"__T":[],"__ROWS":null}}`), nil
	}
	if r.URL.Path != "/app_api.php" {
		return nil, fmt.Errorf("unexpected endpoint in fixture request")
	}
	f.seenCookie = r.Header.Get("Cookie")
	if err := r.ParseForm(); err != nil {
		return nil, err
	}
	page := r.Form.Get("page")
	if f.code != 0 && (f.failPage == 0 || page == fmt.Sprint(f.failPage)) {
		if f.code == -1 {
			return fixtureResponse("{"), nil
		}
		return fixtureResponse(fmt.Sprintf(`{"code":%d,"msg":"fixture business error"}`, f.code)), nil
	}
	value := f.page1
	if page == "2" {
		value = f.page2
	}
	b, _ := json.Marshal(value)
	// 两个 TID 共用脱敏结构；新楼层与楼中楼都使用真实 PID 自然键。
	var copy map[string]any
	_ = json.Unmarshal(b, &copy)
	if f.newPosts {
		if page == "2" {
			post := map[string]any{"tid": 1001, "pid": 4004, "lou": 9, "postdatetimestamp": 1767225900, "content": "New reply", "author": map[string]any{"uid": 2005, "username": "fixture user"}}
			copy["result"] = append(copy["result"].([]any), post)
		} else {
			parent := copy["result"].([]any)[1].(map[string]any)
			comment := map[string]any{"tid": 1001, "pid": 5003, "lou": 0, "postdatetimestamp": 1767225900, "content": "New comment on an existing reply", "author": map[string]any{"uid": 2005, "username": "fixture commenter"}}
			parent["comments"] = append(parent["comments"].([]any), comment)
		}
	}
	b, _ = json.Marshal(copy)
	b = bytes.ReplaceAll(b, []byte(`"tid":1001`), []byte(`"tid":`+r.Form.Get("tid")))
	return fixtureResponse(string(b)), nil
}

func fixtureResponse(body string) *http.Response {
	return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(body)), Header: make(http.Header)}
}

func send(t *testing.T, router http.Handler, method, path, token string, data any) *httptest.ResponseRecorder {
	t.Helper()
	var body io.Reader
	contentType := "application/json"
	if form, ok := data.(url.Values); ok {
		body, contentType = strings.NewReader(form.Encode()), "application/x-www-form-urlencoded"
	} else if data != nil {
		b, err := json.Marshal(data)
		if err != nil {
			t.Fatal(err)
		}
		body = bytes.NewReader(b)
	}
	req := httptest.NewRequest(method, path, body)
	req.Header.Set("Content-Type", contentType)
	if token != "" {
		req.Header.Set("Authorization", "Bearer "+token)
	}
	r := httptest.NewRecorder()
	router.ServeHTTP(r, req)
	if r.Header().Get("X-Request-ID") == "" {
		t.Fatal("response is missing a trace ID")
	}
	return r
}

func payload[T any](t *testing.T, response *httptest.ResponseRecorder) (value T) {
	t.Helper()
	if err := json.Unmarshal(response.Body.Bytes(), &value); err != nil {
		t.Fatalf("decode response: %v %s", err, response.Body.String())
	}
	return value
}

func runWatch(t *testing.T, router http.Handler, token string, id int64) repository.Run {
	t.Helper()
	var response *httptest.ResponseRecorder
	for deadline := time.Now().Add(5 * time.Second); time.Now().Before(deadline); {
		response = send(t, router, "POST", fmt.Sprintf("/api/v1/watches/%d/run", id), token, nil)
		if response.Code != 409 {
			break
		}
		time.Sleep(10 * time.Millisecond)
	}
	expectStatus(t, response, 202)
	run := payload[repository.Run](t, response)
	for deadline := time.Now().Add(10 * time.Second); time.Now().Before(deadline); {
		response = request(t, router, "GET", fmt.Sprintf("/api/v1/runs/%d", run.ID), token)
		expectStatus(t, response, 200)
		run = payload[repository.Run](t, response)
		if run.Status != "running" {
			return run
		}
		time.Sleep(25 * time.Millisecond)
	}
	t.Fatal("collection did not finish within the fixture timeout")
	return run
}

func createWatch(t *testing.T, router http.Handler, token string, tid int64, mode string) repository.Watch {
	t.Helper()
	r := send(t, router, "POST", "/api/v1/watches", token, service.WatchInput{TID: tid, Label: "fixture", InitMode: mode})
	expectStatus(t, r, 201)
	return payload[repository.Watch](t, r)
}

func TestThreadMonitoringWorkflow(t *testing.T) {
	cfg := testConfig(t)
	cfg.BackgroundEnabled = true
	var logs bytes.Buffer
	f := fixtureNGA(t)
	router, store, monitor := openAppWithTransport(t, cfg, &logs, f)
	ctx := context.Background()
	fullCookie := "ngaPassportUid=2001; ngaPassportCid=fixture-valid-secret; browser=fixture-browser-secret"
	response := send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, service.AccountInput{Cookie: fullCookie})
	expectStatus(t, response, 200)
	account, err := store.Account(ctx)
	if err != nil {
		t.Fatal(err)
	}
	ciphertext := bytes.Clone(account.Cookie)
	response = send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, service.AccountInput{PassportUID: "2001", PassportCID: "fixture-invalid-secret"})
	expectStatus(t, response, 422)
	account, err = store.Account(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if account.Status != "valid" || !bytes.Equal(ciphertext, account.Cookie) || account.LastError == "" {
		t.Fatal("invalid replacement changed existing credentials or omitted the validation error")
	}
	response = send(t, router, "POST", "/admin/watches", "", url.Values{"tid": {"1001"}, "init_mode": {"full"}, "label": {"Fixture thread"}})
	expectStatus(t, response, 303)
	if response.Header().Get("Location") != "/admin/watches/1" {
		t.Fatal("creation did not redirect to the watch detail page")
	}
	f.mu.Lock()
	f.code, f.failPage = 51, 2
	f.mu.Unlock()
	run := runWatch(t, router, cfg.APIToken, 1)
	if run.Status != "skipped_pending" || run.Pages != 1 || run.Saved != 4 || !run.Silent {
		t.Fatalf("unexpected incomplete initialization result: %+v", run)
	}
	watch, err := store.Watch(ctx, 1)
	if err != nil {
		t.Fatal(err)
	}
	if watch.BaselineComplete || watch.CursorFloor != 0 {
		t.Fatalf("failure completed the baseline prematurely: %+v", watch)
	}
	f.mu.Lock()
	f.code = 0
	f.mu.Unlock()
	run = runWatch(t, router, cfg.APIToken, 1)
	if run.Status != "success" || run.Saved != 1 || !run.Silent {
		t.Fatalf("rerun did not reuse saved content: %+v", run)
	}
	watch, _ = store.Watch(ctx, 1)
	if !watch.BaselineComplete || watch.CursorFloor != 7 {
		t.Fatalf("cursor does not reflect the actual floor: %+v", watch)
	}
	run = runWatch(t, router, cfg.APIToken, 1)
	if run.Saved != 0 || run.Silent {
		t.Fatalf("rerun saved duplicates or did not enter incremental collection: %+v", run)
	}
	f.mu.Lock()
	f.newPosts = true
	f.mu.Unlock()
	run = runWatch(t, router, cfg.APIToken, 1)
	if run.Status != "success" || run.Saved != 2 {
		t.Fatalf("new reply or comment on an existing reply was not saved: %+v", run)
	}
	posts, total, err := store.Posts(ctx, 1001, 1)
	if err != nil || total != 7 || len(posts) != 7 {
		t.Fatalf("incorrect post deduplication: %d %v", total, err)
	}
	response = request(t, router, "GET", "/admin/threads/1001", "")
	expectStatus(t, response, 200)
	if strings.Contains(response.Body.String(), "<script>fixture-only</script>") || !strings.Contains(response.Body.String(), "&lt;script&gt;") {
		t.Fatal("plain-text preview did not escape HTML")
	}
	for _, action := range []string{"pause", "resume", "reset"} {
		response = send(t, router, "POST", "/admin/watches/1/"+action, "", url.Values{"init_mode": {"full"}})
		expectStatus(t, response, 303)
		watch, _ = store.Watch(ctx, 1)
		if action == "pause" && !watch.Paused || action == "resume" && watch.Paused || action == "reset" && watch.BaselineComplete {
			t.Fatalf("%s did not take effect: %+v", action, watch)
		}
	}
	response = send(t, router, "POST", "/admin/watches/1", "", url.Values{"tid": {"1001"}, "init_mode": {"full"}, "label": {"Updated label"}})
	expectStatus(t, response, 303)
	response = request(t, router, "GET", "/admin/watches/1", "")
	expectStatus(t, response, 200)
	if !strings.Contains(response.Body.String(), "Updated label") || !strings.Contains(response.Body.String(), run.TraceID) {
		t.Fatal("detail page did not display configuration and log trace ID")
	}
	response = send(t, router, "POST", "/admin/watches/1/delete", "", nil)
	expectStatus(t, response, 303)
	_, total, _ = store.Posts(ctx, 1001, 1)
	if total != 7 {
		t.Fatal("deleting the watch removed saved content")
	}
	expectStatus(t, request(t, router, "GET", "/admin/threads/1001", ""), 200)
	// 新目标的 from-now 基线不导入历史；下一轮只保存水位之后的新楼层。
	f.mu.Lock()
	f.newPosts = false
	f.mu.Unlock()
	watch = createWatch(t, router, cfg.APIToken, 1002, "from_now")
	run = runWatch(t, router, cfg.APIToken, watch.ID)
	_, total, _ = store.Posts(ctx, 1002, 1)
	if run.Status != "success" || !run.Silent || total != 0 {
		t.Fatalf("from-now imported historical content: %+v total=%d", run, total)
	}
	f.mu.Lock()
	f.newPosts = true
	f.mu.Unlock()
	run = runWatch(t, router, cfg.APIToken, watch.ID)
	if run.Status != "success" || run.Saved != 1 || run.Silent {
		t.Fatalf("from-now did not save the subsequent reply: %+v", run)
	}
	monitor.Close()
	f.mu.Lock()
	usedCookie := f.seenCookie
	f.mu.Unlock()
	if usedCookie != fullCookie {
		t.Fatal("rejected credentials affected collection or the full Cookie was lost")
	}
	response = request(t, router, "GET", "/api/v1/nga-account", cfg.APIToken)
	for _, secret := range []string{"fixture-valid-secret", "fixture-invalid-secret", "fixture-browser-secret"} {
		if bytes.Contains(logs.Bytes(), []byte(secret)) || strings.Contains(response.Body.String(), secret) || bytes.Contains(ciphertext, []byte(secret)) {
			t.Fatal("credentials appeared in logs, API responses, or plaintext storage")
		}
	}
	assertCollectionChain(t, logs.Bytes(), run.TraceID, run.SourceTraceID)
	// 使用相同数据库和部署密钥重启；账号和 from-now 边界均可继续读取。
	if err = store.Close(ctx); err != nil {
		t.Fatal(err)
	}
	router, restarted, _ := openAppWithTransport(t, cfg, io.Discard, f)
	account, err = restarted.Account(ctx)
	if err != nil || !bytes.Equal(account.Cookie, ciphertext) {
		t.Fatal("credentials were lost after restart")
	}
	response = send(t, router, "POST", "/api/v1/nga-account/check", cfg.APIToken, nil)
	expectStatus(t, response, 200)
	watch, _ = restarted.Watch(ctx, watch.ID)
	if !watch.BaselineComplete || watch.HistoryFloor != 7 || watch.CursorFloor != 9 {
		t.Fatalf("baseline boundaries were lost after restart: %+v", watch)
	}
}

func assertCollectionChain(t *testing.T, raw []byte, trace, source string) {
	t.Helper()
	seen := map[string]bool{}
	starts := map[string]bool{}
	ended := map[string]bool{}
	parents := []string{}
	sourceFound := false
	for _, event := range events(t, raw) {
		if event["trace_id"] == source && event["event"] == "http" {
			sourceFound = true
		}
		if event["trace_id"] != trace {
			continue
		}
		operation, _ := event["operation"].(string)
		seen[operation] = true
		id, _ := event["span_id"].(string)
		if event["event"] == "start" {
			starts[id] = true
			parent, _ := event["parent_span_id"].(string)
			if parent != "" {
				parents = append(parents, parent)
			}
		}
		if event["event"] == "end" {
			ended[id] = true
		}
	}
	for _, operation := range []string{"service.collect_thread", "infrastructure.nga.thread_page", "infrastructure.nga.http", "repository.transaction", "repository.insert_posts", "repository.save_watch"} {
		if !seen[operation] {
			t.Fatalf("collection trace is missing %s", operation)
		}
	}
	if !sourceFound {
		t.Fatal("collection trace is missing its originating HTTP request")
	}
	for id := range starts {
		if !ended[id] {
			t.Fatalf("operation is missing its end log: %s", id)
		}
	}
	for _, id := range parents {
		if !starts[id] {
			t.Fatalf("operation is missing parent span: %s", id)
		}
	}
}

func TestCollectionFailuresKeepCommittedCursor(t *testing.T) {
	cfg := testConfig(t)
	cfg.BackgroundEnabled = true
	var logs bytes.Buffer
	f := fixtureNGA(t)
	router, store, monitor := openAppWithTransport(t, cfg, &logs, f)
	ctx := context.Background()
	credentials := service.AccountInput{PassportUID: "2001", PassportCID: "fixture-valid-secret"}
	expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, credentials), 200)
	watch := createWatch(t, router, cfg.APIToken, 1001, "full")
	if run := runWatch(t, router, cfg.APIToken, watch.ID); run.Status != "success" {
		t.Fatal(run)
	}
	for _, scenario := range []struct {
		code   int
		status string
	}{{51, "skipped_pending"}, {-1, "failed"}, {14, "missing"}, {46, "auth_paused"}} {
		f.mu.Lock()
		f.code, f.failPage = scenario.code, 0
		f.mu.Unlock()
		run := runWatch(t, router, cfg.APIToken, watch.ID)
		if run.Status != scenario.status || run.Error == "" || run.FinishedAt == nil {
			t.Fatalf("business or decoding failure was reported as success: %+v", run)
		}
		persisted, err := store.Watch(ctx, watch.ID)
		if err != nil || !persisted.BaselineComplete || persisted.CursorFloor != 7 {
			t.Fatalf("failure changed the committed cursor: %+v %v", persisted, err)
		}
		if scenario.code == 14 && persisted.State != "missing" || scenario.code == 46 && persisted.State != "auth_paused" {
			t.Fatalf("watch state did not distinguish the business error: %+v", persisted)
		}
		if scenario.code == 14 {
			f.mu.Lock()
			f.code = 0
			f.mu.Unlock()
			if recovered := runWatch(t, router, cfg.APIToken, watch.ID); recovered.Status != "success" {
				t.Fatalf("manual collection did not recover after the thread became accessible: %+v", recovered)
			}
		}
	}
	account, _ := store.Account(ctx)
	if account.Status != "auth_paused" {
		t.Fatal("authentication failure did not pause the account")
	}
	expectStatus(t, send(t, router, "POST", "/api/v1/watches/1/run", cfg.APIToken, nil), 422)
	expectStatus(t, send(t, router, "POST", "/api/v1/watches/1/pause", cfg.APIToken, nil), 200)
	expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, credentials), 200)
	watch, _ = store.Watch(ctx, watch.ID)
	if watch.State != "ready" || !watch.Paused {
		t.Fatalf("validation did not clear the authentication pause or overwrote the user pause: %+v", watch)
	}
	f.mu.Lock()
	f.code = 0
	f.mu.Unlock()
	if run := runWatch(t, router, cfg.APIToken, watch.ID); run.Status != "success" || run.Saved != 0 {
		t.Fatalf("rerun after recovery did not deduplicate content: %+v", run)
	}
	monitor.Close()
	for _, event := range events(t, logs.Bytes()) {
		if event["error"] != nil && (event["stack"] == nil || event["causes"] == nil) {
			t.Fatal("error log is missing causes or stack")
		}
	}
}

func TestVerificationModeAndInterruptedRun(t *testing.T) {
	t.Run("verification mode makes no requests", func(t *testing.T) {
		cfg := testConfig(t)
		f := fixtureNGA(t)
		router, _, _ := openAppWithTransport(t, cfg, io.Discard, f)
		watch := createWatch(t, router, cfg.APIToken, 1001, "full")
		expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, service.AccountInput{PassportUID: "2001", PassportCID: "fixture-valid-secret"}), 409)
		expectStatus(t, send(t, router, "POST", fmt.Sprintf("/api/v1/watches/%d/run", watch.ID), cfg.APIToken, nil), 409)
		if f.calls != 0 {
			t.Fatal("verification mode contacted NGA")
		}
	})
	t.Run("shutdown interrupts collection and permits rerun after restart", func(t *testing.T) {
		cfg := testConfig(t)
		cfg.BackgroundEnabled = true
		f := fixtureNGA(t)
		router, store, monitor := openAppWithTransport(t, cfg, io.Discard, f)
		credentials := service.AccountInput{PassportUID: "2001", PassportCID: "fixture-valid-secret"}
		expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, credentials), 200)
		watch := createWatch(t, router, cfg.APIToken, 1001, "full")
		block := make(chan struct{})
		f.mu.Lock()
		f.block = block
		f.mu.Unlock()
		r := send(t, router, "POST", "/api/v1/watches/1/run", cfg.APIToken, nil)
		expectStatus(t, r, 202)
		run := payload[repository.Run](t, r)
		expectStatus(t, send(t, router, "POST", "/api/v1/watches/1/run", cfg.APIToken, nil), 409)
		expectStatus(t, send(t, router, "DELETE", "/api/v1/watches/1", cfg.APIToken, nil), 409)
		monitor.Close()
		run, err := store.Run(context.Background(), run.ID)
		if err != nil || run.Status != "interrupted" {
			t.Fatalf("run still appears active after shutdown: %+v %v", run, err)
		}
		watch, _ = store.Watch(context.Background(), watch.ID)
		if watch.BaselineComplete || watch.CursorFloor != 0 {
			t.Fatal("interruption advanced the baseline prematurely")
		}
		// 模拟未执行退出清理的旧 running 行；重启只标记中断，不自动补抓。
		stale := repository.Run{TID: 1001, WatchID: watch.ID, Status: "running", StartedAt: time.Now().UTC()}
		if err = store.SaveRun(context.Background(), &stale); err != nil {
			t.Fatal(err)
		}
		if err = store.Close(context.Background()); err != nil {
			t.Fatal(err)
		}
		close(block)
		router, reopened, _ := openAppWithTransport(t, cfg, io.Discard, f)
		stale, _ = reopened.Run(context.Background(), stale.ID)
		if stale.Status != "interrupted" {
			t.Fatal("restart did not mark the previous run as interrupted")
		}
		if run = runWatch(t, router, cfg.APIToken, watch.ID); run.Status != "success" {
			t.Fatal(run)
		}
	})
}
