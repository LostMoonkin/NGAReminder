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
	topicPages []int
}

func (f *oldUserTopicsFixture) RoundTrip(r *http.Request) (*http.Response, error) {
	if r.URL.Path != "/thread.php" || r.URL.Query().Get("authorid") != "2001" || r.URL.Query().Get("searchpost") == "1" {
		return f.userFixture.RoundTrip(r)
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	page, _ := strconv.Atoi(r.URL.Query().Get("page"))
	f.topicPages = append(f.topicPages, page)
	items := make([]any, 1)
	for i := range items {
		id := 1000 - (page-1)*20 - i
		items[i] = map[string]any{"tid": id, "authorid": 2001, "postdate": 1767225600 - (page-1)*20 - i}
	}
	raw, _ := json.Marshal(map[string]any{"code": 0, "result": map[string]any{"__T": items, "__ROWS": 2, "__T__ROWS_PAGE": 1}})
	return fixtureResponse(string(raw)), nil
}

func TestUserIncrementalStopsAfterOldTopicPages(t *testing.T) {
	cfg := testConfig(t)
	cfg.BackgroundEnabled = true
	f := &oldUserTopicsFixture{userFixture: userNGA(t)}
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
	f.mu.Lock()
	defer f.mu.Unlock()
	if run.Status != "success" || run.Saved != 0 || run.Pages != 3 || len(f.topicPages) != 1 {
		t.Fatalf("UID increment scanned old history: status=%s saved=%d pages=%d topic_pages=%v", run.Status, run.Saved, run.Pages, f.topicPages)
	}
}
