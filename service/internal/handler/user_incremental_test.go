package handler

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"strconv"
	"testing"

	"ngareminder/service/internal/repository"
	"ngareminder/service/internal/service"
)

type oldUserTopicsFixture struct {
	*userFixture
	topicPages    []int
	totalPages    int
	itemsPerPage  int
	busyTail      bool
	newOnBoundary bool
}

func (f *oldUserTopicsFixture) RoundTrip(r *http.Request) (*http.Response, error) {
	if r.URL.Path != "/thread.php" || r.URL.Query().Get("authorid") != "2001" || r.URL.Query().Get("searchpost") == "1" {
		return f.userFixture.RoundTrip(r)
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	page, _ := strconv.Atoi(r.URL.Query().Get("page"))
	f.topicPages = append(f.topicPages, page)
	if f.busyTail && page > 1 {
		return fixtureResponse(`{"code":2048,"msg":"服务器忙,请稍后重试"}`), nil
	}
	items := make([]any, f.itemsPerPage)
	for i := range items {
		offset := (page-1)*f.itemsPerPage + i
		items[i] = map[string]any{"tid": 1000 - offset, "authorid": 2001, "postdate": 1767225600 - offset}
	}
	if f.newOnBoundary && page == 1 {
		items[len(items)-1] = map[string]any{"tid": 1004, "authorid": 2001, "postdate": 1767225600}
	}
	raw, _ := json.Marshal(map[string]any{"code": 0, "result": map[string]any{"__T": items, "__ROWS": f.totalPages * f.itemsPerPage, "__T__ROWS_PAGE": f.itemsPerPage}})
	return fixtureResponse(string(raw)), nil
}

func TestUserIncrementalStopsAfterOldTopicPages(t *testing.T) {
	for _, scenario := range []struct {
		name         string
		totalPages   int
		itemsPerPage int
		busyTail     bool
		knownReplies bool
		baseline     bool
	}{
		{name: "minimal", totalPages: 2, itemsPerPage: 1},
		{name: "eight_old_pages", totalPages: 8, itemsPerPage: 20},
		{name: "busy_history_known_reply_total", totalPages: 8, itemsPerPage: 20, busyTail: true, knownReplies: true},
		{name: "baseline", totalPages: 8, itemsPerPage: 20, busyTail: true, baseline: true},
	} {
		t.Run(scenario.name, func(t *testing.T) {
			cfg := testConfig(t)
			cfg.BackgroundEnabled = true
			f := &oldUserTopicsFixture{userFixture: userNGA(t), totalPages: scenario.totalPages, itemsPerPage: scenario.itemsPerPage, busyTail: scenario.busyTail}
			// 满页且最新回复恰好等于水位；已知或未知总数都不能继续读旧历史。
			result := f.replies["result"].(map[string]any)
			result["__R__ROWS_PAGE"] = 1
			if scenario.knownReplies {
				result["__ROWS"] = 160
			}
			router, store, _ := openAppWithTransport(t, cfg, io.Discard, f)
			expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, service.AccountInput{PassportUID: "2009", PassportCID: "fixture-valid-secret"}), 200)
			response := send(t, router, "POST", "/api/v1/watches", cfg.APIToken, service.WatchInput{Kind: "uid", UID: 2001})
			expectStatus(t, response, 201)
			watch := payload[repository.Watch](t, response)
			watch.BaselineComplete = !scenario.baseline
			if watch.BaselineComplete {
				watch.TopicCursor = repository.UserCursor{Timestamp: 1767225600, ID: 1000}
				watch.ReplyCursor = repository.UserCursor{Timestamp: 1767225720, ID: 4002}
			}
			if err := store.SaveWatch(context.Background(), &watch); err != nil {
				t.Fatal(err)
			}
			run := runWatch(t, router, cfg.APIToken, watch.ID)
			f.mu.Lock()
			defer f.mu.Unlock()
			if run.Status != "success" || run.Saved != 0 || run.Pages != 3 || len(f.topicPages) != 1 || run.Silent != scenario.baseline {
				t.Fatalf("UID increment scanned old history: status=%s saved=%d pages=%d topic_pages=%v", run.Status, run.Saved, run.Pages, f.topicPages)
			}
			after, err := store.Watch(context.Background(), watch.ID)
			if err != nil || !after.BaselineComplete || after.TopicCursor.ID != 1000 || after.ReplyCursor.ID != 4002 {
				t.Fatalf("incorrect watermarks after old-page collection: %+v %v", after, err)
			}
		})
	}
}

func TestUserIncrementalProcessesEntireBoundaryPage(t *testing.T) {
	cfg := testConfig(t)
	cfg.BackgroundEnabled = true
	f := &oldUserTopicsFixture{userFixture: userNGA(t), totalPages: 8, itemsPerPage: 2, busyTail: true, newOnBoundary: true}
	result := f.replies["result"].(map[string]any)
	result["__ROWS"], result["__R__ROWS_PAGE"] = 160, 2
	result["__T"] = append(result["__T"].([]any), map[string]any{"__P": map[string]any{"tid": 1003, "pid": 4003, "authorid": 2001, "postdate": 1767225720}})
	router, store, _ := openAppWithTransport(t, cfg, io.Discard, f)
	expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, service.AccountInput{PassportUID: "2009", PassportCID: "fixture-valid-secret"}), 200)
	response := send(t, router, "POST", "/api/v1/watches", cfg.APIToken, service.WatchInput{Kind: "uid", UID: 2001})
	expectStatus(t, response, 201)
	watch := payload[repository.Watch](t, response)
	watch.BaselineComplete = true
	watch.TopicCursor = repository.UserCursor{Timestamp: 1767225600, ID: 1000}
	watch.ReplyCursor = repository.UserCursor{Timestamp: 1767225720, ID: 4002}
	if err := store.SaveWatch(context.Background(), &watch); err != nil {
		t.Fatal(err)
	}
	run := runWatch(t, router, cfg.APIToken, watch.ID)
	after, err := store.Watch(context.Background(), watch.ID)
	if err != nil || run.Status != "success" || run.Saved != 2 || run.Pages != 5 || after.TopicCursor.ID != 1004 || after.ReplyCursor.ID != 4003 {
		t.Fatalf("same-second candidates after the boundary were lost: run=%+v watch=%+v err=%v", run, after, err)
	}
}
