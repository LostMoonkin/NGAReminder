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
	p["pid"], p["lou"], p["content"] = 4003, 7, `<script>fixture-only</script>[b]第 7 楼[/b]`
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
		return nil, fmt.Errorf("测试中出现非契约路径")
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
			post := map[string]any{"tid": 1001, "pid": 4004, "lou": 9, "postdatetimestamp": 1767225900, "content": "新楼层", "author": map[string]any{"uid": 2005, "username": "fixture user"}}
			copy["result"] = append(copy["result"].([]any), post)
		} else {
			parent := copy["result"].([]any)[1].(map[string]any)
			comment := map[string]any{"tid": 1001, "pid": 5003, "lou": 0, "postdatetimestamp": 1767225900, "content": "旧楼层下的新评论", "author": map[string]any{"uid": 2005, "username": "fixture commenter"}}
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
		t.Fatal("响应缺少关联 ID")
	}
	return r
}

func payload[T any](t *testing.T, response *httptest.ResponseRecorder) (value T) {
	t.Helper()
	if err := json.Unmarshal(response.Body.Bytes(), &value); err != nil {
		t.Fatalf("解析响应：%v %s", err, response.Body.String())
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
	t.Fatal("采集没有在本地 fixture 时限内完成")
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
		t.Fatal("无效替换破坏了原有凭据或缺少校验错误")
	}
	response = send(t, router, "POST", "/admin/watches", "", url.Values{"tid": {"1001"}, "init_mode": {"full"}, "label": {"中文主题"}})
	expectStatus(t, response, 303)
	if response.Header().Get("Location") != "/admin/watches/1" {
		t.Fatal("创建后未跳转监控详情")
	}
	f.mu.Lock()
	f.code, f.failPage = 51, 2
	f.mu.Unlock()
	run := runWatch(t, router, cfg.APIToken, 1)
	if run.Status != "skipped_pending" || run.Pages != 1 || run.Saved != 4 || !run.Silent {
		t.Fatalf("不完整初始化结果：%+v", run)
	}
	watch, err := store.Watch(ctx, 1)
	if err != nil {
		t.Fatal(err)
	}
	if watch.BaselineComplete || watch.CursorFloor != 0 {
		t.Fatalf("失败提前完成基线：%+v", watch)
	}
	f.mu.Lock()
	f.code = 0
	f.mu.Unlock()
	run = runWatch(t, router, cfg.APIToken, 1)
	if run.Status != "success" || run.Saved != 1 || !run.Silent {
		t.Fatalf("重跑没有复用已保存内容：%+v", run)
	}
	watch, _ = store.Watch(ctx, 1)
	if !watch.BaselineComplete || watch.CursorFloor != 7 {
		t.Fatalf("未按实际楼层设置水位：%+v", watch)
	}
	run = runWatch(t, router, cfg.APIToken, 1)
	if run.Saved != 0 || run.Silent {
		t.Fatalf("重跑重复保存或未进入增量：%+v", run)
	}
	f.mu.Lock()
	f.newPosts = true
	f.mu.Unlock()
	run = runWatch(t, router, cfg.APIToken, 1)
	if run.Status != "success" || run.Saved != 2 {
		t.Fatalf("新回复和旧楼层下新增评论未保存：%+v", run)
	}
	posts, total, err := store.Posts(ctx, 1001, 1)
	if err != nil || total != 7 || len(posts) != 7 {
		t.Fatalf("帖子去重不正确 %d %v", total, err)
	}
	response = request(t, router, "GET", "/admin/threads/1001", "")
	expectStatus(t, response, 200)
	if strings.Contains(response.Body.String(), "<script>fixture-only</script>") || !strings.Contains(response.Body.String(), "&lt;script&gt;") {
		t.Fatal("纯文本预览没有转义 HTML")
	}
	for _, action := range []string{"pause", "resume", "reset"} {
		response = send(t, router, "POST", "/admin/watches/1/"+action, "", url.Values{"init_mode": {"full"}})
		expectStatus(t, response, 303)
		watch, _ = store.Watch(ctx, 1)
		if action == "pause" && !watch.Paused || action == "resume" && watch.Paused || action == "reset" && watch.BaselineComplete {
			t.Fatalf("%s 未生效：%+v", action, watch)
		}
	}
	response = send(t, router, "POST", "/admin/watches/1", "", url.Values{"tid": {"1001"}, "init_mode": {"full"}, "label": {"已修改备注"}})
	expectStatus(t, response, 303)
	response = request(t, router, "GET", "/admin/watches/1", "")
	expectStatus(t, response, 200)
	if !strings.Contains(response.Body.String(), "已修改备注") || !strings.Contains(response.Body.String(), run.TraceID) {
		t.Fatal("详情未展示配置和日志关联 ID")
	}
	response = send(t, router, "POST", "/admin/watches/1/delete", "", nil)
	expectStatus(t, response, 303)
	_, total, _ = store.Posts(ctx, 1001, 1)
	if total != 7 {
		t.Fatal("删除监控删除了已保存内容")
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
		t.Fatalf("from-now 导入了历史：%+v total=%d", run, total)
	}
	f.mu.Lock()
	f.newPosts = true
	f.mu.Unlock()
	run = runWatch(t, router, cfg.APIToken, watch.ID)
	if run.Status != "success" || run.Saved != 1 || run.Silent {
		t.Fatalf("from-now 后续新回复未保存：%+v", run)
	}
	monitor.Close()
	f.mu.Lock()
	usedCookie := f.seenCookie
	f.mu.Unlock()
	if usedCookie != fullCookie {
		t.Fatal("失败的候选凭据影响了采集或完整 Cookie 被丢弃")
	}
	response = request(t, router, "GET", "/api/v1/nga-account", cfg.APIToken)
	for _, secret := range []string{"fixture-valid-secret", "fixture-invalid-secret", "fixture-browser-secret"} {
		if bytes.Contains(logs.Bytes(), []byte(secret)) || strings.Contains(response.Body.String(), secret) || bytes.Contains(ciphertext, []byte(secret)) {
			t.Fatal("凭据出现在日志、API 或明文存储中")
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
		t.Fatal("重启后丢失凭据")
	}
	response = send(t, router, "POST", "/api/v1/nga-account/check", cfg.APIToken, nil)
	expectStatus(t, response, 200)
	watch, _ = restarted.Watch(ctx, watch.ID)
	if !watch.BaselineComplete || watch.HistoryFloor != 7 || watch.CursorFloor != 9 {
		t.Fatalf("重启丢失边界：%+v", watch)
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
			t.Fatalf("采集 trace 缺少 %s", operation)
		}
	}
	if !sourceFound {
		t.Fatal("采集 trace 缺少来源 HTTP 请求")
	}
	for id := range starts {
		if !ended[id] {
			t.Fatalf("操作缺少结束日志 %s", id)
		}
	}
	for _, id := range parents {
		if !starts[id] {
			t.Fatalf("操作缺少父 span %s", id)
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
			t.Fatalf("业务/解析失败被误判成功：%+v", run)
		}
		persisted, err := store.Watch(ctx, watch.ID)
		if err != nil || !persisted.BaselineComplete || persisted.CursorFloor != 7 {
			t.Fatalf("失败改变了原有效水位：%+v %v", persisted, err)
		}
		if scenario.code == 14 && persisted.State != "missing" || scenario.code == 46 && persisted.State != "auth_paused" {
			t.Fatalf("监控状态未区分业务错误：%+v", persisted)
		}
		if scenario.code == 14 {
			f.mu.Lock()
			f.code = 0
			f.mu.Unlock()
			if recovered := runWatch(t, router, cfg.APIToken, watch.ID); recovered.Status != "success" {
				t.Fatalf("主题恢复可见后不能手动恢复采集：%+v", recovered)
			}
		}
	}
	account, _ := store.Account(ctx)
	if account.Status != "auth_paused" {
		t.Fatal("认证失效未暂停账号")
	}
	expectStatus(t, send(t, router, "POST", "/api/v1/watches/1/run", cfg.APIToken, nil), 422)
	expectStatus(t, send(t, router, "POST", "/api/v1/watches/1/pause", cfg.APIToken, nil), 200)
	expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, credentials), 200)
	watch, _ = store.Watch(ctx, watch.ID)
	if watch.State != "ready" || !watch.Paused {
		t.Fatalf("校验成功未解除认证暂停，或覆盖了用户暂停：%+v", watch)
	}
	f.mu.Lock()
	f.code = 0
	f.mu.Unlock()
	if run := runWatch(t, router, cfg.APIToken, watch.ID); run.Status != "success" || run.Saved != 0 {
		t.Fatalf("恢复后不能去重重跑：%+v", run)
	}
	monitor.Close()
	for _, event := range events(t, logs.Bytes()) {
		if event["error"] != nil && (event["stack"] == nil || event["causes"] == nil) {
			t.Fatal("错误日志缺少原因或 stack")
		}
	}
}

func TestVerificationModeAndInterruptedRun(t *testing.T) {
	t.Run("核验模式不发请求", func(t *testing.T) {
		cfg := testConfig(t)
		f := fixtureNGA(t)
		router, _, _ := openAppWithTransport(t, cfg, io.Discard, f)
		watch := createWatch(t, router, cfg.APIToken, 1001, "full")
		expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, service.AccountInput{PassportUID: "2001", PassportCID: "fixture-valid-secret"}), 409)
		expectStatus(t, send(t, router, "POST", fmt.Sprintf("/api/v1/watches/%d/run", watch.ID), cfg.APIToken, nil), 409)
		if f.calls != 0 {
			t.Fatal("核验模式访问了 NGA")
		}
	})
	t.Run("停止中断采集并允许重启重跑", func(t *testing.T) {
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
			t.Fatalf("停止后仍显示运行中：%+v %v", run, err)
		}
		watch, _ = store.Watch(context.Background(), watch.ID)
		if watch.BaselineComplete || watch.CursorFloor != 0 {
			t.Fatal("中断提前推进基线")
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
			t.Fatal("重启没有标记上次中断的任务")
		}
		if run = runWatch(t, router, cfg.APIToken, watch.ID); run.Status != "success" {
			t.Fatal(run)
		}
	})
}
