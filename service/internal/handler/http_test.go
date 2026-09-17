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
	router, store, _ := openAppWithTransport(t, cfg, logs, nil)
	return router, store
}

func openAppWithTransport(t *testing.T, cfg config.Config, logs io.Writer, transport http.RoundTripper) (*gin.Engine, *repository.Store, *service.Monitoring) {
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
	monitor, err := service.NewMonitoring(ctx, cfg, store, infrastructure.NewNGA(cfg.NGAUserAgent, transport), log, infrastructure.NewNotifier(transport))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(monitor.Close)
	router, err := New(admin, monitor, log)
	if err != nil {
		t.Fatal(err)
	}
	return router, store, monitor
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
		t.Fatal("every response must include X-Request-ID")
	}
	return recorder
}

func expectStatus(t *testing.T, response *httptest.ResponseRecorder, want int) {
	t.Helper()
	if response.Code != want {
		t.Fatalf("HTTP status %d, want %d, response %s", response.Code, want, response.Body.String())
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
	if dashboard.Code != 200 {
		t.Log(logs.String())
	}
	expectStatus(t, dashboard, 200)
	if !strings.Contains(dashboard.Body.String(), "后台任务已关闭") {
		t.Fatal("admin page is missing the migration verification notice")
	}
	if len(dashboard.Result().Cookies()) != 0 || strings.Contains(dashboard.Body.String(), "/admin/login") {
		t.Fatal("LAN admin page must not create sessions or show a login entry")
	}
	assertChain(t, logs.Bytes(), dashboard.Header().Get("X-Request-ID"))
	settings := request(t, router, "GET", "/api/v1/settings", cfg.APIToken)
	expectStatus(t, settings, 200)
	var payload map[string]any
	if err := json.Unmarshal(settings.Body.Bytes(), &payload); err != nil {
		t.Fatal(err)
	}
	if payload["timezone"] != "Asia/Shanghai" || payload["background_enabled"] != false || payload["database_status"] != "ok" {
		t.Fatalf("incorrect runtime settings: %v", payload)
	}
	for _, field := range []string{"api_token", "encryption_key"} {
		if _, exists := payload[field]; exists {
			t.Fatalf("API exposed secret field %s", field)
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
			t.Fatal("logs contain credentials")
		}
		if strings.Contains(dashboard.Body.String(), secret) {
			t.Fatal("admin page contains credentials")
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
		t.Fatal("panic response exposed internal details")
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
			t.Fatal("panic error was not redacted")
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
		t.Fatal("rejected requests must still include a trace ID")
	}
}

func events(t *testing.T, data []byte) []map[string]any {
	t.Helper()
	var result []map[string]any
	for _, line := range bytes.Split(bytes.TrimSpace(data), []byte("\n")) {
		var event map[string]any
		if err := json.Unmarshal(line, &event); err != nil {
			t.Fatalf("logs must contain valid JSON: %v", err)
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
			t.Fatal("call chain is missing start/end, result, or duration")
		}
		if parent, _ := e["parent_span_id"].(string); parent != "" && starts[parent] == nil {
			t.Fatal("call chain references an unknown parent operation")
		}
	}
	if !layers["handler"] || !layers["service"] || !layers["repository"] || !sawSQL {
		t.Fatal("request logs do not cover handler, service, repository, and SQL")
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
			t.Fatal("error is missing causes or a stack trace")
		}
		found := false
		for _, raw := range stack {
			f := raw.(map[string]any)
			if f["file"] == "" || f["line"].(float64) == 0 {
				t.Fatal("error stack is missing file or line information")
			}
			found = found || strings.Contains(f["func"].(string), function)
		}
		if !found {
			t.Fatalf("error stack did not preserve origin %s", function)
		}
	}
	if count != 1 {
		t.Fatalf("request error must be logged once, got %d entries", count)
	}
}
