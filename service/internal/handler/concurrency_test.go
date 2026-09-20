package handler

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strconv"
	"sync"
	"testing"
	"time"

	"ngareminder/service/internal/repository"
	"ngareminder/service/internal/service"
)

type concurrentNGA struct {
	mu           sync.Mutex
	active, peak map[string]int
	started      chan string
	block        map[string]<-chan struct{}
	failPage     int
	slow         bool
}

func (f *concurrentNGA) RoundTrip(r *http.Request) (*http.Response, error) {
	if err := r.ParseForm(); err != nil {
		return nil, err
	}
	target := r.Form.Get("tid")
	if target == "" {
		target = "uid:" + r.Form.Get("authorid")
	}
	if r.URL.Path == "/nuke.php" {
		target = "uid:" + r.Form.Get("uid")
	}
	page, _ := strconv.Atoi(r.Form.Get("page"))
	f.mu.Lock()
	if f.active == nil {
		f.active, f.peak = map[string]int{}, map[string]int{}
	}
	f.active[target]++
	f.peak[target] = max(f.peak[target], f.active[target])
	block, fail, slow := f.block[target], f.failPage, f.slow
	f.mu.Unlock()
	defer func() { f.mu.Lock(); f.active[target]--; f.mu.Unlock() }()
	if f.started != nil {
		f.started <- target
	}
	if block != nil {
		select {
		case <-block:
		case <-r.Context().Done():
			return nil, r.Context().Err()
		}
	}
	if r.URL.Path == "/nuke.php" {
		return fixtureResponse(fmt.Sprintf(`<script>var __UCPUSER = {"uid":%s,"username":"Fixture"};</script>`, r.Form.Get("uid"))), nil
	}
	if r.URL.Path == "/thread.php" {
		return fixtureResponse(`{"code":0,"result":{"__T":[],"__ROWS":0,"__R__ROWS_PAGE":20,"__T__ROWS_PAGE":20}}`), nil
	}
	if page == fail {
		return fixtureResponse(`{"code":46}`), nil
	}
	if slow && page > 1 {
		timer := time.NewTimer(750 * time.Millisecond)
		defer timer.Stop()
		select {
		case <-timer.C:
		case <-r.Context().Done():
			return nil, r.Context().Err()
		}
	}
	tid, _ := strconv.ParseInt(target, 10, 64)
	pid := 4000 + page
	if page == 1 {
		pid = 0
	}
	body, _ := json.Marshal(map[string]any{
		"code": 0, "currentPage": page, "totalPage": 5, "perPage": 1, "vrows": 5,
		"result": []any{map[string]any{"tid": tid, "pid": pid, "lou": page - 1, "subject": "Fixture", "content": fmt.Sprintf("Page %d", page), "author": map[string]any{"uid": 2001, "username": "Fixture"}}},
	})
	return fixtureResponse(string(body)), nil
}

func TestHistoryConcurrencyConfigurationAndCollection(t *testing.T) {
	cfg := testConfig(t)
	cfg.BackgroundEnabled = true
	f := &concurrentNGA{slow: true}
	router, store, monitor := openAppWithTransport(t, cfg, io.Discard, f)
	expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, service.AccountInput{PassportUID: "2001", PassportCID: "fixture-valid-secret"}), 200)
	form := url.Values{"kind": {"tid"}, "tid": {"1001"}, "init_mode": {"full"}, "history_concurrency": {"2"}}
	expectStatus(t, send(t, router, "POST", "/admin/watches", "", form), 303)
	watch, err := store.Watch(context.Background(), 1)
	if err != nil || watch.HistoryConcurrency != 2 {
		t.Fatal("form did not persist concurrency", err)
	}
	for _, value := range []int{-1, 0, 17} {
		expectStatus(t, send(t, router, "PUT", "/api/v1/watches/1", cfg.APIToken, service.WatchInput{TID: 1001, InitMode: "full", HistoryConcurrency: &value}), 400)
	}
	run := runWatch(t, router, cfg.APIToken, 1)
	if run.Status != "success" || run.Pages != 5 || run.Saved != 5 {
		t.Fatalf("incomplete parallel baseline: %+v", run)
	}
	f.mu.Lock()
	peak := f.peak["1001"]
	f.mu.Unlock()
	if peak != 2 {
		t.Fatalf("expected exactly two overlapping page requests, got %d", peak)
	}
	events, err := store.Inbox(context.Background(), 1)
	if err != nil || len(events) != 0 {
		t.Fatal("parallel baseline produced notifications", err)
	}
	// API 省略新字段应保留值，修改并发数不能重置已有水位。
	response := send(t, router, "PUT", "/api/v1/watches/1", cfg.APIToken, service.WatchInput{TID: 1001, InitMode: "full"})
	expectStatus(t, response, 200)
	if payload[repository.Watch](t, response).HistoryConcurrency != 2 {
		t.Fatal("omitted concurrency field reset the setting")
	}
	one := 1
	response = send(t, router, "PUT", "/api/v1/watches/1", cfg.APIToken, service.WatchInput{TID: 1001, InitMode: "full", HistoryConcurrency: &one})
	expectStatus(t, response, 200)
	watch = payload[repository.Watch](t, response)
	if !watch.BaselineComplete || watch.CursorFloor != 4 || watch.HistoryConcurrency != 1 {
		t.Fatal("concurrency update changed the baseline")
	}
	expectStatus(t, send(t, router, "PUT", "/api/v1/watches/1", cfg.APIToken, service.WatchInput{TID: 1001, InitMode: "full"}), 200)
	expectStatus(t, send(t, router, "POST", "/api/v1/watches/1/reset", cfg.APIToken, map[string]string{"init_mode": "full"}), 200)
	f.mu.Lock()
	f.peak["1001"] = 0
	f.mu.Unlock()
	run = runWatch(t, router, cfg.APIToken, 1)
	f.mu.Lock()
	peak = f.peak["1001"]
	f.mu.Unlock()
	if run.Status != "success" || run.Saved != 0 || peak != 1 {
		t.Fatal("serial rerun differed from parallel collection", run, peak)
	}
	three := 3
	expectStatus(t, send(t, router, "PUT", "/api/v1/watches/1", cfg.APIToken, service.WatchInput{TID: 1001, InitMode: "full", HistoryConcurrency: &three}), 200)
	monitor.Close()
	if err := store.Close(context.Background()); err != nil {
		t.Fatal(err)
	}
	_, reopened, _ := openAppWithTransport(t, cfg, io.Discard, f)
	watch, err = reopened.Watch(context.Background(), 1)
	if err != nil || watch.HistoryConcurrency != 3 || !watch.BaselineComplete {
		t.Fatal("concurrency or progress lost on restart", err)
	}
}

func TestIndependentWatchesAndAuthenticationPause(t *testing.T) {
	cfg := testConfig(t)
	cfg.BackgroundEnabled = true
	f := &concurrentNGA{}
	router, store, monitor := openAppWithTransport(t, cfg, io.Discard, f)
	expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, service.AccountInput{PassportUID: "2001", PassportCID: "fixture-valid-secret"}), 200)
	createWatch(t, router, cfg.APIToken, 1001, "full")
	createWatch(t, router, cfg.APIToken, 1002, "full")
	expectStatus(t, send(t, router, "POST", "/api/v1/watches", cfg.APIToken, service.WatchInput{Kind: "uid", UID: 2001}), 201)
	block := make(chan struct{})
	uidBlock := make(chan struct{})
	f.mu.Lock()
	f.block = map[string]<-chan struct{}{"1001": block, "1002": block, "uid:2001": uidBlock}
	f.started = make(chan string, 32)
	f.mu.Unlock()
	response := send(t, router, "POST", "/api/v1/watches/1/run", cfg.APIToken, nil)
	expectStatus(t, response, 202)
	if err := monitor.Tick(context.Background(), time.Now()); err != nil {
		t.Fatal(err)
	}
	if err := monitor.Tick(context.Background(), time.Now()); err != nil {
		t.Fatal(err)
	}
	seen := map[string]bool{}
	for len(seen) < 3 {
		select {
		case target := <-f.started:
			seen[target] = true
		case <-time.After(5 * time.Second):
			t.Fatal("different watches did not overlap", seen)
		}
	}
	for _, id := range []int64{1, 2, 3} {
		expectStatus(t, send(t, router, "POST", fmt.Sprintf("/api/v1/watches/%d/run", id), cfg.APIToken, nil), 409)
		runs, err := store.WatchRuns(context.Background(), id)
		if err != nil || len(runs) != 1 {
			t.Fatal("duplicate scheduled run", id, err)
		}
	}
	// 模拟一个运行认证失败：其他已经在途的成功响应不得把暂停状态写回 ready。
	f.mu.Lock()
	f.failPage = 2
	f.mu.Unlock()
	close(block)
	for id := int64(1); id <= 2; id++ {
		awaitRun(t, store, id)
	}
	close(uidBlock)
	if run := awaitRun(t, store, 3); run.Status != "success" {
		t.Fatal("in-flight UID baseline failed", run)
	}
	account, err := store.Account(context.Background())
	if err != nil || account.Status != "auth_paused" {
		t.Fatal("authentication failure did not pause account", err)
	}
	watches, err := store.Watches(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	for _, watch := range watches {
		if watch.State != "auth_paused" {
			t.Fatal("concurrent finish cleared authentication pause", watch.ID)
		}
	}
}

func TestParallelPageFailureAndShutdown(t *testing.T) {
	for _, stop := range []bool{false, true} {
		t.Run(fmt.Sprintf("shutdown_%t", stop), func(t *testing.T) {
			cfg := testConfig(t)
			cfg.BackgroundEnabled = true
			f := &concurrentNGA{slow: true}
			router, store, monitor := openAppWithTransport(t, cfg, io.Discard, f)
			expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, service.AccountInput{PassportUID: "2001", PassportCID: "fixture-valid-secret"}), 200)
			three := 3
			expectStatus(t, send(t, router, "POST", "/api/v1/watches", cfg.APIToken, service.WatchInput{TID: 1001, InitMode: "full", HistoryConcurrency: &three}), 201)
			f.mu.Lock()
			f.started = make(chan string, 32)
			if !stop {
				f.failPage = 3
			}
			f.mu.Unlock()
			response := send(t, router, "POST", "/api/v1/watches/1/run", cfg.APIToken, nil)
			expectStatus(t, response, 202)
			run := payload[repository.Run](t, response)
			if stop {
				for range 3 {
					select {
					case <-f.started:
					case <-time.After(5 * time.Second):
						t.Fatal("parallel requests did not start")
					}
				}
				monitor.Close()
			}
			run = awaitRun(t, store, run.ID)
			want := "auth_paused"
			if stop {
				want = "interrupted"
			}
			watch, err := store.Watch(context.Background(), 1)
			if err != nil || run.Status != want || watch.BaselineComplete || watch.CursorFloor != 0 || run.Pages != 1 {
				t.Fatal("failed batch advanced progress", watch, run, err)
			}
			monitor.Close()
			f.mu.Lock()
			if f.active["1001"] != 0 {
				t.Error("page requests leaked after stop")
			}
			f.failPage = 0
			f.slow = false
			f.started = nil
			f.mu.Unlock()
			if err := store.Close(context.Background()); err != nil {
				t.Fatal(err)
			}
			router, _, _ = openAppWithTransport(t, cfg, io.Discard, f)
			expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, service.AccountInput{PassportUID: "2001", PassportCID: "fixture-valid-secret"}), 200)
			if recovered := runWatch(t, router, cfg.APIToken, 1); recovered.Status != "success" || recovered.Saved != 4 {
				t.Fatal("failed baseline did not resume without duplicates", recovered)
			}
		})
	}
}
