package handler

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
	"sync"
	"testing"
	"time"

	"ngareminder/service/internal/repository"
	"ngareminder/service/internal/service"
)

type userRequest struct{ path, page, tid, pid, uid string }
type userFixture struct {
	profile                                  string
	mu                                       sync.Mutex
	topics1, topics2, replies, thread, reply map[string]any
	live                                     bool
	knownReplies                             bool
	failReplies                              string
	failPID                                  string
	failCode                                 int
	calls                                    []userRequest
}

func userNGA(t *testing.T) *userFixture {
	return &userFixture{topics1: readFixture(t, "user_topics_page_1"), topics2: readFixture(t, "user_topics_page_2"), replies: readFixture(t, "user_replies_success"), thread: readFixture(t, "thread_comments_hot_post"), reply: readFixture(t, "post_by_pid_success"), profile: `<script>var __UCPUSER = {"uid":2001,"username":"Fixture username"};</script>`}
}

func (f *userFixture) RoundTrip(r *http.Request) (*http.Response, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if err := r.ParseForm(); err != nil {
		return nil, err
	}
	call := userRequest{path: r.URL.Path, page: r.Form.Get("page"), tid: r.Form.Get("tid"), pid: r.Form.Get("pid"), uid: r.Form.Get("authorid")}
	f.calls = append(f.calls, call)
	encode := func(value any) *http.Response { b, _ := json.Marshal(value); return fixtureResponse(string(b)) }
	clone := func(value map[string]any) map[string]any {
		b, _ := json.Marshal(value)
		var copy map[string]any
		_ = json.Unmarshal(b, &copy)
		return copy
	}
	if r.URL.Path == "/nuke.php" {
		if r.URL.Query().Get("func") != "ucp" || r.URL.Query().Get("uid") != "2001" || r.Header.Get("Referer") != r.URL.String() {
			return nil, fmt.Errorf("user profile request differs from Rust")
		}
		return fixtureResponse(f.profile), nil
	}
	if r.URL.Path == "/thread.php" {
		if strings.Contains(r.Header.Get("Cookie"), "unrelated=") {
			return nil, fmt.Errorf("user search received unrelated cookies")
		}
		if call.uid == "2009" {
			return fixtureResponse(`{"code":0}`), nil
		}
		if call.uid != "2001" {
			return nil, fmt.Errorf("unexpected watched UID")
		}
		replies := r.Form.Get("searchpost") == "1"
		if replies && f.failReplies != "" {
			switch f.failReplies {
			case "auth":
				return fixtureResponse(`{"code":2048,"msg":"必须登录"}`), nil
			case "json":
				return fixtureResponse(`{`), nil
			case "empty":
				return fixtureResponse(`{"code":0,"result":{"__T":[],"__ROWS":0,"__R__ROWS_PAGE":20}}`), nil
			default:
				response := fixtureResponse("")
				response.StatusCode = 503
				return response, nil
			}
		}
		if replies {
			if call.page == "1" && f.live {
				items := []any{}
				for _, pid := range []int{4003, 4004} {
					items = append(items, map[string]any{"__P": map[string]any{"tid": 1003, "pid": pid, "authorid": 2001, "postdate": 1767226000}})
				}
				items = append(items, map[string]any{"__P": map[string]any{"tid": 1003, "pid": 999, "postdate": ""}})
				var total any
				if f.knownReplies {
					total = 100
				}
				return encode(map[string]any{"code": 0, "result": map[string]any{"__T": items, "__ROWS": total, "__R__ROWS_PAGE": 2}}), nil
			}
			if f.live && call.page == "2" {
				value := clone(f.replies)
				result := value["result"].(map[string]any)
				result["__R__ROWS_PAGE"] = 2
				result["__T"] = append([]any{map[string]any{"__P": map[string]any{"tid": 1003, "pid": 4005, "authorid": 2001, "postdate": 1767225999}}}, result["__T"].([]any)...)
				return encode(value), nil
			}
			if call.page == "3" {
				response := fixtureResponse("")
				response.StatusCode = 503
				return response, nil
			}
			return encode(f.replies), nil
		}
		if call.page == "1" {
			value := clone(f.topics1)
			result := value["result"].(map[string]any)
			if f.live {
				result["__T"] = []any{
					map[string]any{"tid": 1004, "authorid": 2001, "postdate": 1767226000},
					map[string]any{"tid": 1005, "authorid": 2001, "postdate": 1767226001},
				}
			} else {
				result["__T"] = result["__T"].([]any)[:1]
			}
			return encode(value), nil
		}
		if call.page == "2" {
			return encode(f.topics2), nil
		}
		return nil, fmt.Errorf("unexpected user list page")
	}
	if r.URL.Path != "/app_api.php" {
		return nil, fmt.Errorf("unexpected NGA endpoint")
	}
	if f.failPID != "" && call.pid == f.failPID {
		code := f.failCode
		if code == 0 {
			code = 51
		}
		return fixtureResponse(fmt.Sprintf(`{"code":%d}`, code)), nil
	}
	if call.pid != "" {
		value := clone(f.reply)
		post := value["result"].([]any)[0].(map[string]any)
		post["tid"], post["pid"], post["lou"] = call.tid, call.pid, 0
		post["author"].(map[string]any)["uid"] = 2001
		if call.pid == "4005" {
			post["author"].(map[string]any)["uid"] = 9999
		}
		return encode(value), nil
	}
	if call.page != "1" {
		return nil, fmt.Errorf("UID collection fetched an entire thread")
	}
	value := clone(f.thread)
	value["totalPage"] = 99
	if call.tid == "1005" {
		value["result"].([]any)[0].(map[string]any)["author"].(map[string]any)["uid"] = 9999
	}
	b, _ := json.Marshal(value)
	b = bytes.ReplaceAll(b, []byte(`"tid":1001`), []byte(`"tid":`+call.tid))
	return fixtureResponse(string(b)), nil
}

func TestUserMonitoringWorkflow(t *testing.T) {
	cfg := testConfig(t)
	cfg.BackgroundEnabled = true
	var logs bytes.Buffer
	f := userNGA(t)
	router, store, monitor := openAppWithTransport(t, cfg, &logs, f)
	credentials := service.AccountInput{Cookie: "ngaPassportUid=2009; ngaPassportCid=fixture-valid-secret; unrelated=fixture-browser-secret"}
	expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, credentials), 200)
	response := send(t, router, "POST", "/admin/watches", "", url.Values{"kind": {"uid"}, "uid": {"2001"}, "label": {"User fixture"}, "interval_seconds": {"60"}, "schedule_present": {"1"}})
	expectStatus(t, response, 303)
	run := runWatch(t, router, cfg.APIToken, 1)
	watch, err := store.Watch(context.Background(), 1)
	if err != nil || run.Status != "success" || !run.Silent || run.Saved != 0 || !watch.BaselineComplete || watch.TopicCursor.ID != 1001 || watch.ReplyCursor.ID != 4002 {
		t.Fatalf("invalid UID baseline: %+v %+v %v", run, watch, err)
	}
	if watch.Title != "Fixture username" || watch.Label != "User fixture" {
		t.Fatalf("UID baseline did not save the profile username separately from the label: title=%q label=%q", watch.Title, watch.Label)
	}
	f.mu.Lock()
	for _, call := range f.calls {
		if call.path == "/app_api.php" {
			t.Error("initialization fetched historical details")
		}
	}
	f.live = true
	f.failPID = "4004"
	f.profile = `<script>var __UCPUSER = {"uid":2001,"username":"Updated username"};</script>`
	f.mu.Unlock()
	run = runWatch(t, router, cfg.APIToken, 1)
	after, _ := store.Watch(context.Background(), 1)
	threads, _ := store.Threads(context.Background())
	if run.Status != "skipped_pending" || after.Title != watch.Title || after.TopicCursor != watch.TopicCursor || after.ReplyCursor != watch.ReplyCursor || len(threads) != 0 {
		t.Fatalf("detail failure committed a partial UID run: %+v %+v", run, after)
	}
	f.mu.Lock()
	f.failPID = ""
	f.knownReplies = true
	f.mu.Unlock()
	// 水位尚未到达时，已知总数的后续空 503 仍是失败，不能当作历史结束。
	beforeTail := watch
	beforeTail.ReplyCursor.Timestamp--
	if err := store.SaveWatch(context.Background(), &beforeTail); err != nil {
		t.Fatal(err)
	}
	run = runWatch(t, router, cfg.APIToken, 1)
	after, _ = store.Watch(context.Background(), 1)
	if run.Status != "failed" || after.ReplyCursor != beforeTail.ReplyCursor || after.TopicCursor != beforeTail.TopicCursor {
		t.Fatal("known-total empty tail advanced UID watermarks", run, after)
	}
	if err := store.SaveWatch(context.Background(), &watch); err != nil {
		t.Fatal(err)
	}
	f.mu.Lock()
	f.knownReplies = false
	f.mu.Unlock()
	run = runWatch(t, router, cfg.APIToken, 1)
	if run.Status != "success" || run.Saved != 3 || run.Silent {
		t.Fatalf("wrong UID increment: %+v", run)
	}
	posts, total, err := store.Posts(context.Background(), 1003, 1)
	if err != nil || total != 2 || len(posts) != 2 {
		t.Fatalf("distinct PID replies were lost: %d %v", total, err)
	}
	for _, p := range posts {
		if p.Kind != "reply" || p.Floor != 0 || p.Key != fmt.Sprintf("pid:%d", p.PID) || p.AuthorUID != 2001 {
			t.Fatalf("incorrect by-PID identity: %+v", p)
		}
	}
	main, total, err := store.Posts(context.Background(), 1004, 1)
	if err != nil || total != 1 || main[0].Kind != "main" {
		t.Fatalf("UID topic saved other participants: %+v %v", main, err)
	}
	_, total, _ = store.Posts(context.Background(), 1005, 1)
	if total != 0 {
		t.Fatal("detail author check did not reject a topic")
	}
	after, _ = store.Watch(context.Background(), 1)
	if after.Title != "Updated username" || after.TopicCursor.ID != 1005 || after.ReplyCursor.ID != 4004 {
		t.Fatalf("separate watermarks not committed: %+v", after)
	}
	if run = runWatch(t, router, cfg.APIToken, 1); run.Status != "success" || run.Saved != 0 {
		t.Fatal("UID repeat run was not idempotent", run)
	}
	response = request(t, router, "GET", "/admin/watches/1", "")
	expectStatus(t, response, 200)
	if !strings.Contains(response.Body.String(), "UID 2001") || !strings.Contains(response.Body.String(), "Updated username") || !strings.Contains(response.Body.String(), "回帖水位") || !strings.Contains(response.Body.String(), run.TraceID) {
		t.Fatal("UID page omitted identity, username, watermarks, or run result")
	}
	// 第二个 UID 与 TID 的配置和运行记录相互独立，tid=0 不再导致唯一索引冲突。
	response = send(t, router, "POST", "/api/v1/watches", cfg.APIToken, service.WatchInput{Kind: "uid", UID: 2002})
	expectStatus(t, response, 201)
	second := payload[repository.Watch](t, response)
	detail := payload[service.WatchDetail](t, request(t, router, "GET", fmt.Sprintf("/api/v1/watches/%d", second.ID), cfg.APIToken))
	if len(detail.Runs) != 0 {
		t.Fatal("UID run history leaked across watches")
	}
	expectStatus(t, send(t, router, "POST", "/api/v1/watches/1/reset", cfg.APIToken, map[string]any{}), 200)
	if run = runWatch(t, router, cfg.APIToken, 1); !run.Silent || run.Saved != 0 || run.Status != "success" {
		t.Fatalf("reset did not silently rebaseline: %+v", run)
	}
	// 运行结果先提交，goroutine 收尾后才释放采集锁；与启动采集一样等待忙碌结束。
	for deadline := time.Now().Add(5 * time.Second); time.Now().Before(deadline); {
		response = send(t, router, "DELETE", "/api/v1/watches/1", cfg.APIToken, nil)
		if response.Code != 409 {
			break
		}
		time.Sleep(10 * time.Millisecond)
	}
	expectStatus(t, response, 200)
	_, total, _ = store.Posts(context.Background(), 1003, 1)
	if total != 2 {
		t.Fatal("reset or deletion removed UID content")
	}
	monitor.Close()
	if bytes.Contains(logs.Bytes(), []byte("fixture-valid-secret")) || bytes.Contains(logs.Bytes(), []byte("fixture-browser-secret")) {
		t.Fatal("UID logs leaked credentials")
	}
	assertUserTrace(t, logs.Bytes(), run.TraceID)
}

func assertUserTrace(t *testing.T, raw []byte, trace string) {
	t.Helper()
	seen := map[string]bool{}
	for _, event := range events(t, raw) {
		if event["trace_id"] == trace {
			operation, _ := event["operation"].(string)
			seen[operation] = true
		}
		if event["error"] != nil && (event["stack"] == nil || event["causes"] == nil) {
			t.Fatal("UID error lacks causes or stack")
		}
	}
	for _, operation := range []string{"service.collect_user", "infrastructure.nga.user_profile", "infrastructure.nga.user_page", "infrastructure.nga.http", "repository.save_watch"} {
		if !seen[operation] {
			t.Errorf("UID trace missing %s", operation)
		}
	}
}

func TestUserBaselineFailureAndAuthRecovery(t *testing.T) {
	cfg := testConfig(t)
	cfg.BackgroundEnabled = true
	f := userNGA(t)
	router, store, monitor := openAppWithTransport(t, cfg, io.Discard, f)
	credentials := service.AccountInput{PassportUID: "2009", PassportCID: "fixture-valid-secret"}
	expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, credentials), 200)
	response := send(t, router, "POST", "/api/v1/watches", cfg.APIToken, service.WatchInput{Kind: "uid", UID: 2001})
	expectStatus(t, response, 201)
	createWatch(t, router, cfg.APIToken, 1001, "full")
	expectStatus(t, send(t, router, "POST", "/api/v1/watches/2/pause", cfg.APIToken, nil), 200)
	f.mu.Lock()
	profile := f.profile
	f.profile = `<html>User not found</html>`
	f.mu.Unlock()
	profileRun := runWatch(t, router, cfg.APIToken, 1)
	profileWatch, _ := store.Watch(context.Background(), 1)
	if profileRun.Status != "missing" || profileWatch.State != "missing" || profileWatch.BaselineComplete || profileWatch.TopicCursor.ID != 0 || profileWatch.ReplyCursor.ID != 0 || profileWatch.Title != "" {
		t.Fatalf("missing profile established a UID baseline: %+v %+v", profileRun, profileWatch)
	}
	f.mu.Lock()
	calls := len(f.calls)
	f.mu.Unlock()
	if err := monitor.Tick(context.Background(), time.Now().Add(time.Hour)); err != nil {
		t.Fatal(err)
	}
	f.mu.Lock()
	afterCalls := len(f.calls)
	f.mu.Unlock()
	if calls != afterCalls {
		t.Fatal("missing user was automatically retried")
	}
	f.mu.Lock()
	f.profile = profile
	f.mu.Unlock()
	for _, failure := range []string{"json", "503", "auth"} {
		f.mu.Lock()
		f.failReplies = failure
		f.mu.Unlock()
		run := runWatch(t, router, cfg.APIToken, 1)
		watch, _ := store.Watch(context.Background(), 1)
		if run.Status == "success" || watch.BaselineComplete || watch.TopicCursor.ID != 0 || watch.ReplyCursor.ID != 0 {
			t.Fatalf("%s established an empty/partial baseline: %+v %+v", failure, run, watch)
		}
	}
	expectStatus(t, send(t, router, "POST", "/api/v1/watches/1/run", cfg.APIToken, nil), 422)
	expectStatus(t, send(t, router, "POST", "/api/v1/watches/1/reset", cfg.APIToken, map[string]any{}), 200)
	expectStatus(t, send(t, router, "POST", "/api/v1/watches/1/run", cfg.APIToken, nil), 422)
	expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, credentials), 200)
	paused, _ := store.Watch(context.Background(), 2)
	if !paused.Paused || paused.State != "ready" {
		t.Fatal("credential recovery changed manual pause", paused)
	}
	expectStatus(t, send(t, router, "POST", "/api/v1/watches/2/run", cfg.APIToken, nil), 409)
	f.mu.Lock()
	f.failReplies = "empty"
	f.mu.Unlock()
	run := runWatch(t, router, cfg.APIToken, 1)
	if run.Status != "success" || !run.Silent || run.Saved != 0 {
		t.Fatal("valid empty replies must permit baseline", run)
	}
	// 遗留水位和静默状态在重启后仍可用。
	watch, _ := store.Watch(context.Background(), 1)
	if !watch.BaselineComplete || watch.TopicCursor.ID != 1001 || watch.ReplyCursor.ID != 0 {
		t.Fatal(watch)
	}
}

// 轮询持久化运行结果，避免用固定延迟猜测异步采集是否完成。
func awaitRun(t *testing.T, store *repository.Store, id int64) repository.Run {
	t.Helper()
	for deadline := time.Now().Add(10 * time.Second); time.Now().Before(deadline); {
		runs, err := store.WatchRuns(context.Background(), id)
		if err != nil {
			t.Fatal(err)
		}
		if len(runs) > 0 && runs[0].Status != "running" {
			return runs[0]
		}
		time.Sleep(20 * time.Millisecond)
	}
	t.Fatal("automatic collection did not finish")
	return repository.Run{}
}
