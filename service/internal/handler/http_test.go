package handler

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"net/url"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gin-gonic/gin"

	"ngareminder/service/internal/config"
	"ngareminder/service/internal/infrastructure"
	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/repository"
	"ngareminder/service/internal/service"
)

func testConfig(t *testing.T) config.Config {
	t.Helper()
	dir := t.TempDir()
	return config.Config{ListenAddress: "0.0.0.0:8989", DatabasePath: filepath.Join(dir, "app.db"), AssetsPath: filepath.Join(dir, "assets"),
		APIToken:      "fake-quoted-\"api-token-for-local-testing",
		EncryptionKey: base64.StdEncoding.EncodeToString(bytes.Repeat([]byte{7}, 32)), Timezone: "Asia/Shanghai"}
}

func openApp(t *testing.T, cfg config.Config, logs io.Writer) (*gin.Engine, *repository.Store) {
	t.Helper()
	log := logging.New(logs)
	log.SetSecrets(cfg.Secrets()...)
	ctx := log.WithContext(context.Background())
	if err := infrastructure.PrepareStorage(ctx, cfg.DatabasePath, cfg.AssetsPath); err != nil {
		t.Fatal(err)
	}
	store, err := repository.Open(ctx, cfg.DatabasePath)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := store.Close(ctx); err != nil {
			t.Error(err)
		}
	})
	admin, err := service.NewAdmin(ctx, cfg, store)
	if err != nil {
		t.Fatal(err)
	}
	router, err := New(admin, log)
	if err != nil {
		t.Fatal(err)
	}
	return router, store
}

func request(t *testing.T, router http.Handler, method, path, bearer string) *httptest.ResponseRecorder {
	t.Helper()
	req := httptest.NewRequest(method, path, nil)
	if bearer != "" {
		req.Header.Set("Authorization", "Bearer "+bearer)
	}
	recorder := httptest.NewRecorder()
	router.ServeHTTP(recorder, req)
	if recorder.Header().Get("X-Request-ID") == "" {
		t.Fatal("每个响应都必须有 X-Request-ID")
	}
	return recorder
}

func expectStatus(t *testing.T, response *httptest.ResponseRecorder, want int) {
	t.Helper()
	if response.Code != want {
		t.Fatalf("HTTP 状态 %d，预期 %d，响应 %s", response.Code, want, response.Body.String())
	}
}

func TestPasswordlessAdminAndRequestChain(t *testing.T) {
	cfg := testConfig(t)
	var logs bytes.Buffer
	router, store := openApp(t, cfg, &logs)
	expectStatus(t, request(t, router, "GET", "/healthz", ""), 200)
	expectStatus(t, request(t, router, "GET", "/readyz", ""), 200)
	expectStatus(t, request(t, router, "GET", "/api/v1/settings", ""), 401)
	expectStatus(t, request(t, router, "GET", "/api/v1/settings", "wrong-token"), 401)
	expectStatus(t, request(t, router, "GET", "/missing", ""), 404)
	expectStatus(t, request(t, router, "GET", "/admin/", ""), 404)
	expectStatus(t, request(t, router, "GET", "/admin/login", ""), 404)
	expectStatus(t, request(t, router, "POST", "/admin/logout", ""), 404)
	expectStatus(t, request(t, router, "POST", "/api/v1/settings", cfg.APIToken), 405)
	dashboard := request(t, router, "GET", "/admin", "")
	expectStatus(t, dashboard, 200)
	if !strings.Contains(dashboard.Body.String(), "后台任务已关闭") {
		t.Fatal("管理页缺少迁移核验模式提示")
	}
	if len(dashboard.Result().Cookies()) != 0 || strings.Contains(dashboard.Body.String(), "登录") {
		t.Fatal("内网管理页不应建立会话或出现登录入口")
	}
	assertChain(t, logs.Bytes(), dashboard.Header().Get("X-Request-ID"))
	settings := request(t, router, "GET", "/api/v1/settings", cfg.APIToken)
	expectStatus(t, settings, 200)
	var payload map[string]any
	if err := json.Unmarshal(settings.Body.Bytes(), &payload); err != nil {
		t.Fatal(err)
	}
	if payload["timezone"] != "Asia/Shanghai" || payload["background_enabled"] != false || payload["database_status"] != "ok" {
		t.Fatalf("运行设置不正确：%v", payload)
	}
	for _, field := range []string{"api_token", "encryption_key"} {
		if _, exists := payload[field]; exists {
			t.Fatalf("API 泄露秘密字段 %s", field)
		}
	}
	assertChain(t, logs.Bytes(), settings.Header().Get("X-Request-ID"))
	expectStatus(t, request(t, router, "GET", "/api/v1/settings?token="+url.QueryEscape(cfg.APIToken), cfg.APIToken), 200)

	if err := store.Close(context.Background()); err != nil {
		t.Fatal(err)
	}
	router, _ = openApp(t, cfg, &logs)
	expectStatus(t, request(t, router, "GET", "/admin", ""), 200)
	for _, secret := range cfg.Secrets() {
		encoded, _ := json.Marshal(secret)
		if bytes.Contains(logs.Bytes(), []byte(secret)) || bytes.Contains(logs.Bytes(), encoded[1:len(encoded)-1]) {
			t.Fatal("日志包含凭据")
		}
		if strings.Contains(dashboard.Body.String(), secret) {
			t.Fatal("管理页包含凭据")
		}
	}
}

func TestFailureAndPanicLogs(t *testing.T) {
	cfg := testConfig(t)
	var logs bytes.Buffer
	router, store := openApp(t, cfg, &logs)
	// 故障路由只注册在测试中，生产二进制没有该入口。
	router.GET("/__test/panic", func(c *gin.Context) { panicInService(c.Request.Context(), cfg.APIToken) })
	response := request(t, router, "GET", "/__test/panic", "")
	expectStatus(t, response, 500)
	assertErrorStack(t, logs.Bytes(), response.Header().Get("X-Request-ID"), "panicInService")
	if strings.Contains(response.Body.String(), "stack") || strings.Contains(response.Body.String(), cfg.APIToken) {
		t.Fatal("panic 响应泄露内部信息")
	}
	if err := store.Close(context.Background()); err != nil {
		t.Fatal(err)
	}
	response = request(t, router, "GET", "/readyz", "")
	expectStatus(t, response, 503)
	assertErrorStack(t, logs.Bytes(), response.Header().Get("X-Request-ID"), "repository.(*Store).Check")
	expectStatus(t, request(t, router, "GET", "/healthz", ""), 200)
	for _, event := range events(t, logs.Bytes()) {
		encoded, _ := json.Marshal(event)
		if strings.Contains(string(encoded), "fake-quoted") {
			t.Fatal("panic 错误信息未脱敏")
		}
	}
}

func panicInService(ctx context.Context, secret string) {
	_, span := logging.Start(ctx, "service.test_panic")
	var err error
	defer span.End(&err)
	panic("synthetic failure: " + secret)
}

func TestCrossOriginWriteRejected(t *testing.T) {
	router, _ := openApp(t, testConfig(t), io.Discard)
	req := httptest.NewRequest("POST", "/api/v1/settings", nil)
	req.Header.Set("Origin", "https://another-site.invalid")
	w := httptest.NewRecorder()
	router.ServeHTTP(w, req)
	expectStatus(t, w, 403)
	if w.Header().Get("X-Request-ID") == "" {
		t.Fatal("被拒绝请求仍应可关联")
	}
}

func events(t *testing.T, data []byte) []map[string]any {
	t.Helper()
	var result []map[string]any
	for _, line := range bytes.Split(bytes.TrimSpace(data), []byte("\n")) {
		var event map[string]any
		if err := json.Unmarshal(line, &event); err != nil {
			t.Fatalf("日志必须是有效 JSON：%v", err)
		}
		result = append(result, event)
	}
	return result
}

func assertChain(t *testing.T, data []byte, traceID string) {
	t.Helper()
	starts, ends := map[string]map[string]any{}, map[string]bool{}
	layers := map[string]bool{}
	sawSQL := false
	for _, e := range events(t, data) {
		if e["trace_id"] != traceID {
			continue
		}
		id, _ := e["span_id"].(string)
		if e["event"] == "start" {
			starts[id] = e
			operation, _ := e["operation"].(string)
			layers[strings.SplitN(operation, ".", 2)[0]] = true
		}
		if e["event"] == "end" {
			ends[id] = e["result"] == "ok" && e["duration_ms"] != nil
		}
		if e["event"] == "sql" {
			sawSQL = true
		}
	}
	for id, e := range starts {
		if id == "" || !ends[id] {
			t.Fatal("调用链缺少开始/结束、结果或耗时")
		}
		if parent, _ := e["parent_span_id"].(string); parent != "" && starts[parent] == nil {
			t.Fatal("调用链父操作无法追溯")
		}
	}
	if !layers["handler"] || !layers["service"] || !layers["repository"] || !sawSQL {
		t.Fatal("请求日志未覆盖 handler、service、repository 和 SQL")
	}
}

func assertErrorStack(t *testing.T, data []byte, traceID, function string) {
	t.Helper()
	count := 0
	for _, e := range events(t, data) {
		if e["trace_id"] != traceID || e["level"] != "error" {
			continue
		}
		count++
		stack, ok := e["stack"].([]any)
		if !ok || len(stack) == 0 || e["error"] == nil || e["causes"] == nil {
			t.Fatal("错误缺少原因链或 stack trace")
		}
		found := false
		for _, raw := range stack {
			f := raw.(map[string]any)
			if f["file"] == "" || f["line"].(float64) == 0 {
				t.Fatal("错误栈缺少文件或行号")
			}
			found = found || strings.Contains(f["func"].(string), function)
		}
		if !found {
			t.Fatalf("错误栈未保留起点 %s", function)
		}
	}
	if count != 1 {
		t.Fatalf("同一个请求错误应打印一次，实际 %d 次", count)
	}
}
