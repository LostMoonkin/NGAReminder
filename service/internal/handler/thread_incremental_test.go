package handler

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"slices"
	"strconv"
	"sync"
	"testing"
	"time"

	"ngareminder/service/internal/repository"
	"ngareminder/service/internal/service"
)

type incrementalThreadFixture struct {
	mu       sync.Mutex
	maxFloor int
	failPage int
	pages    []int
}

func (f *incrementalThreadFixture) RoundTrip(r *http.Request) (*http.Response, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if r.URL.Path == "/thread.php" {
		return fixtureResponse(`{"code":0,"result":{"__T":[],"__ROWS":0}}`), nil
	}
	if err := r.ParseForm(); err != nil {
		return nil, err
	}
	page, err := strconv.Atoi(r.Form.Get("page"))
	if err != nil {
		return nil, err
	}
	f.pages = append(f.pages, page)
	if page == f.failPage {
		return fixtureResponse(`{"code":51}`), nil
	}
	posts := []map[string]any{}
	for floor := (page - 1) * 2; floor <= f.maxFloor && floor < page*2; floor++ {
		posts = append(posts, map[string]any{
			"tid": 1001, "pid": 4000 + floor, "lou": floor, "content": fmt.Sprintf("Floor %d", floor),
			"author": map[string]any{"uid": 2001, "username": "fixture"},
		})
	}
	body, _ := json.Marshal(map[string]any{
		"code": 0, "currentPage": page, "totalPage": f.maxFloor/2 + 1,
		"perPage": 2, "vrows": f.maxFloor + 1, "tsubject": "Incremental fixture", "result": posts,
	})
	return fixtureResponse(string(body)), nil
}

func (f *incrementalThreadFixture) expectPages(t *testing.T, want ...int) {
	t.Helper()
	f.mu.Lock()
	defer f.mu.Unlock()
	if !slices.Equal(f.pages, want) {
		t.Fatalf("requested pages = %v, want %v; incremental collection must not rescan history", f.pages, want)
	}
	f.pages = nil
}

func TestThreadIncrementalRequestsOnlyChangedTail(t *testing.T) {
	cfg := testConfig(t)
	cfg.BackgroundEnabled = true
	f := &incrementalThreadFixture{maxFloor: 11}
	router, store, monitor := openAppWithTransport(t, cfg, io.Discard, f)
	expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken,
		service.AccountInput{PassportUID: "2001", PassportCID: "fixture-valid-secret"}), 200)
	watch := createWatch(t, router, cfg.APIToken, 1001, "from_now")
	if run := runWatch(t, router, cfg.APIToken, watch.ID); run.Status != "success" || run.Saved != 0 {
		t.Fatalf("baseline failed: %+v", run)
	}
	f.expectPages(t, 1, 6)
	if run := runWatch(t, router, cfg.APIToken, watch.ID); run.Status != "success" || run.Saved != 0 {
		t.Fatalf("unchanged thread failed: %+v", run)
	}
	f.expectPages(t, 1)

	f.mu.Lock()
	f.maxFloor = 12
	f.mu.Unlock()
	if run := runWatch(t, router, cfg.APIToken, watch.ID); run.Status != "success" || run.Saved != 1 {
		t.Fatalf("new tail reply was not collected: %+v", run)
	}
	f.expectPages(t, 1, 6, 7)
	current, err := store.Watch(context.Background(), watch.ID)
	if err != nil || current.CursorFloor != 12 {
		t.Fatalf("new tail cursor = %+v, error = %v", current, err)
	}

	// 未跨页的新楼层也必须通过 vrows 的变化发现。
	f.mu.Lock()
	f.maxFloor = 13
	f.mu.Unlock()
	if run := runWatch(t, router, cfg.APIToken, watch.ID); run.Status != "success" || run.Saved != 1 {
		t.Fatalf("same-page reply was not collected: %+v", run)
	}
	f.expectPages(t, 1, 7)

	// 尾部后页失败时保留已经保存的正文，但不提交水位和远端分页快照。
	f.mu.Lock()
	f.maxFloor, f.failPage = 17, 9
	f.mu.Unlock()
	if run := runWatch(t, router, cfg.APIToken, watch.ID); run.Status != "skipped_pending" || run.Saved != 2 {
		t.Fatalf("failed tail run did not retain partial content: %+v", run)
	}
	f.expectPages(t, 1, 7, 8, 9)
	current, err = store.Watch(context.Background(), watch.ID)
	if err != nil || current.CursorFloor != 13 || current.RemoteRows != 14 || current.RemoteTotalPages != 7 {
		t.Fatalf("failed run advanced progress: %+v, error = %v", current, err)
	}
	f.mu.Lock()
	f.failPage = 2 // 无关历史页失败不能阻止正常增量。
	f.mu.Unlock()
	if run := runWatch(t, router, cfg.APIToken, watch.ID); run.Status != "success" || run.Saved != 2 {
		t.Fatalf("retry lost tail replies or duplicated saved content: %+v", run)
	}
	f.expectPages(t, 1, 7, 8, 9)

	monitor.Close()
	if err := store.Close(context.Background()); err != nil {
		t.Fatal(err)
	}
	router, store, _ = openAppWithTransport(t, cfg, io.Discard, f)
	if run := runWatch(t, router, cfg.APIToken, watch.ID); run.Status != "success" || run.Saved != 0 {
		t.Fatalf("restart lost the pagination snapshot: %+v", run)
	}
	f.expectPages(t, 1)

	// 旧 Go 数据没有远端快照，只补查水位所在的尾页，不能重建全量基线。
	current, err = store.Watch(context.Background(), watch.ID)
	if err != nil {
		t.Fatal(err)
	}
	current.RemoteRows, current.RemoteTotalPages = 0, 0
	if err := store.SaveWatch(context.Background(), &current); err != nil {
		t.Fatal(err)
	}
	if run := runWatch(t, router, cfg.APIToken, watch.ID); run.Status != "success" || run.Saved != 0 || run.Silent {
		t.Fatalf("legacy progress triggered a full initialization: %+v", run)
	}
	f.expectPages(t, 1, 9)
}

func TestThreadGapRecoveryRequestsOnlyDueNeighborPages(t *testing.T) {
	cfg := testConfig(t)
	cfg.BackgroundEnabled = true
	f := &incrementalThreadFixture{maxFloor: 99, failPage: 2}
	router, store, monitor := openAppWithTransport(t, cfg, io.Discard, f)
	ctx := context.Background()
	expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken,
		service.AccountInput{PassportUID: "2001", PassportCID: "fixture-valid-secret"}), 200)
	watch := createWatch(t, router, cfg.APIToken, 1001, "from_now")
	if run := runWatch(t, router, cfg.APIToken, watch.ID); run.Status != "success" {
		t.Fatalf("baseline failed: %+v", run)
	}
	f.expectPages(t, 1, 50)
	anchor := time.Now().UTC()
	future := anchor.Add(time.Hour)
	watch, _ = store.Watch(ctx, watch.ID)
	watch.NextRunAt = &future
	if err := store.SaveWatch(ctx, &watch); err != nil {
		t.Fatal(err)
	}
	gaps := []repository.FloorGap{
		{WatchID: watch.ID, Floor: 20, PageHint: 11, Status: "pending", FirstSeen: anchor, Deadline: future},
		{WatchID: watch.ID, Floor: 21, PageHint: 11, Status: "pending", FirstSeen: anchor, Deadline: future},
		{WatchID: watch.ID, Floor: 60, Status: "pending", FirstSeen: anchor.Add(4 * time.Minute), Deadline: future},
	}
	if err := store.AddFloorGaps(ctx, gaps); err != nil {
		t.Fatal(err)
	}
	run := tickRun(t, monitor, store, watch.ID, anchor.Add(5*time.Minute))
	if run.Source != "gap_recovery" || run.Status != "success" || run.Saved != 2 || run.Pages != 4 {
		t.Fatalf("targeted gap recovery failed: %+v", run)
	}
	f.expectPages(t, 1, 10, 11, 12)
	current, err := store.Watch(ctx, watch.ID)
	if err != nil || current.CursorFloor != 99 || current.RemoteRows != 100 || current.RemoteTotalPages != 50 || !current.NextRunAt.Equal(future) {
		t.Fatalf("gap recovery changed live progress: %+v, error = %v", current, err)
	}

	// 正常增量与补偿同时到期时也只合并新增尾页和到期缺口附近页。
	due := anchor.Add(9 * time.Minute)
	current.NextRunAt = &due
	if err := store.SaveWatch(ctx, &current); err != nil {
		t.Fatal(err)
	}
	f.mu.Lock()
	f.maxFloor = 101
	f.mu.Unlock()
	run = tickRun(t, monitor, store, watch.ID, due)
	if run.Source != "automatic" || run.Status != "success" || run.Saved != 3 || run.Pages != 6 {
		t.Fatalf("combined tail/gap collection failed: %+v", run)
	}
	f.expectPages(t, 1, 50, 51, 30, 31, 32)
	gaps, err = store.FloorGaps(ctx, watch.ID)
	if err != nil || len(gaps) != 3 {
		t.Fatalf("unexpected gaps: %+v, error = %v", gaps, err)
	}
	for _, gap := range gaps {
		if gap.Status != "resolved" {
			t.Fatalf("due floor was not recovered: %+v", gap)
		}
	}
}
