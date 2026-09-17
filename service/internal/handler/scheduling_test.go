package handler

import (
	"bytes"
	"context"
	"fmt"
	"io"
	"net/url"
	"strings"
	"testing"
	"time"

	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/repository"
	"ngareminder/service/internal/service"
)

func TestSchedulingSkipsAndManualBypass(t *testing.T) {
	cfg := testConfig(t)
	cfg.BackgroundEnabled = true
	var logs bytes.Buffer
	f := fixtureNGA(t)
	router, store, monitor := openAppWithTransport(t, cfg, &logs, f)
	expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, service.AccountInput{PassportUID: "2001", PassportCID: "fixture-valid-secret"}), 200)
	form := url.Values{"kind": {"tid"}, "tid": {"1001"}, "init_mode": {"full"}, "interval_seconds": {"60"}, "schedule_present": {"1"},
		"interval_weekdays": {"1"}, "interval_start": {"23:00"}, "interval_end": {"02:00"}, "interval_override_seconds": {"300"},
		"period_weekdays": {"1", "2"}, "period_start": {"23:00", "01:00"}, "period_end": {"02:00", "04:00"}}
	expectStatus(t, send(t, router, "POST", "/admin/watches", "", form), 303)
	watch, err := store.Watch(context.Background(), 1)
	if err != nil || len(watch.IntervalRules) != 1 || len(watch.NoFetchPeriods) != 2 || watch.IntervalRules[0].IntervalSeconds != 300 {
		t.Fatalf("form schedule did not persist: %+v %v", watch, err)
	}
	location, err := time.LoadLocation(cfg.Timezone)
	if err != nil {
		t.Fatal(err)
	}
	now := time.Date(2026, 9, 14, 23, 30, 0, 0, location)
	due := now.Add(-24 * time.Hour).UTC()
	watch.NextRunAt = &due
	if err = store.SaveWatch(context.Background(), &watch); err != nil {
		t.Fatal(err)
	}
	log := logging.New(&logs)
	ctx, span := logging.Start(log.WithContext(context.Background()), "fixture.scheduler")
	if err = monitor.Tick(ctx, now); err != nil {
		t.Fatal(err)
	}
	span.End(&err)
	runs, _ := store.WatchRuns(context.Background(), 1)
	saved, _ := store.Watch(context.Background(), 1)
	end := time.Date(2026, 9, 15, 4, 0, 0, 0, location)
	if len(runs) != 1 || runs[0].Status != "skipped_no_fetch" || runs[0].Source != "automatic" || runs[0].FinishedAt == nil || runs[0].TraceID == "" || runs[0].SourceTraceID != logging.TraceID(ctx) || saved.NextRunAt == nil || !saved.NextRunAt.Equal(end) || saved.BaselineComplete || saved.CursorFloor != 0 {
		t.Fatalf("incorrect automatic skip: %+v %+v", runs, saved)
	}
	f.mu.Lock()
	calls := f.calls
	f.mu.Unlock()
	if calls != 1 {
		t.Fatalf("no-fetch contacted NGA: %d calls", calls)
	}
	if err = monitor.Tick(context.Background(), now.Add(time.Hour)); err != nil {
		t.Fatal(err)
	}
	runs, _ = store.WatchRuns(context.Background(), 1)
	if len(runs) != 1 {
		t.Fatal("created repeated skips inside one continuous window")
	}
	// 当前实际时间也设置为全天免拉取，确认 HTTP 手动入口仍会访问 NGA。
	saved.NoFetchPeriods = []repository.TimeWindow{{Weekdays: []int{1, 2, 3, 4, 5, 6, 7}, Start: "00:00", End: "00:00"}}
	due = time.Now().Add(-time.Hour).UTC()
	saved.NextRunAt = &due
	if err = store.SaveWatch(context.Background(), &saved); err != nil {
		t.Fatal(err)
	}
	if err = monitor.Tick(ctx, time.Now()); err != nil {
		t.Fatal(err)
	}
	run := runWatch(t, router, cfg.APIToken, 1)
	if run.Status != "success" || run.Source != "manual" || run.Saved != 5 {
		t.Fatalf("manual run did not bypass no-fetch: %+v", run)
	}
	// 手动运行后仍保持本段跳过，下次 Tick 不新增跳过记录。
	before, _ := store.WatchRuns(context.Background(), 1)
	if err = monitor.Tick(ctx, time.Now().Add(time.Minute)); err != nil {
		t.Fatal(err)
	}
	runs, _ = store.WatchRuns(context.Background(), 1)
	if len(runs) != len(before) {
		t.Fatal("manual run caused duplicate no-fetch skips")
	}
	response := request(t, router, "GET", "/admin/watches/1", "")
	expectStatus(t, response, 200)
	if !strings.Contains(response.Body.String(), "每周全天免拉取") || !strings.Contains(response.Body.String(), "免拉取，本轮跳过") {
		t.Fatal("watch page omitted suppression status")
	}
	// 在同一 watch 上修改频率不重建基线；API 明确空数组可以清空表单配置的规则。
	seconds := 30
	response = send(t, router, "PUT", "/api/v1/watches/1", cfg.APIToken, service.WatchInput{TID: 1001, InitMode: "full", IntervalSeconds: &seconds, IntervalRules: []repository.IntervalRule{}, NoFetchPeriods: []repository.TimeWindow{}})
	expectStatus(t, response, 200)
	saved = payload[repository.Watch](t, response)
	if !saved.BaselineComplete || saved.CursorFloor != 7 || len(saved.NoFetchPeriods) != 0 || len(saved.IntervalRules) != 0 {
		t.Fatal("schedule edit changed progress or failed to clear rules", saved)
	}
	expectStatus(t, send(t, router, "POST", "/api/v1/watches/1/pause", cfg.APIToken, nil), 200)
	expectStatus(t, send(t, router, "POST", "/api/v1/watches/1/run", cfg.APIToken, nil), 409)
	before, _ = store.WatchRuns(context.Background(), 1)
	if err = monitor.Tick(ctx, time.Now().Add(time.Hour)); err != nil {
		t.Fatal(err)
	}
	runs, _ = store.WatchRuns(context.Background(), 1)
	if len(runs) != len(before) {
		t.Fatal("scheduler ran a paused watch")
	}
	expectStatus(t, send(t, router, "POST", "/api/v1/watches/1/resume", cfg.APIToken, nil), 200)
	if err = monitor.Tick(ctx, time.Now()); err != nil {
		t.Fatal(err)
	}
	run = awaitRun(t, store, 1)
	if run.Source != "automatic" || run.Status != "success" {
		t.Fatal("resumed watch did not run automatically", run)
	}
	monitor.Close()
	for _, event := range events(t, logs.Bytes()) {
		if event["error"] != nil && (event["stack"] == nil || event["causes"] == nil) {
			t.Fatal("scheduler error is missing a stack")
		}
	}
}

func TestSchedulerRestartAndVerificationMode(t *testing.T) {
	cfg := testConfig(t)
	cfg.BackgroundEnabled = true
	f := userNGA(t)
	router, store, monitor := openAppWithTransport(t, cfg, io.Discard, f)
	expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, service.AccountInput{PassportUID: "2009", PassportCID: "fixture-valid-secret"}), 200)
	response := send(t, router, "POST", "/api/v1/watches", cfg.APIToken, service.WatchInput{Kind: "uid", UID: 2001})
	expectStatus(t, response, 201)
	if run := runWatch(t, router, cfg.APIToken, 1); run.Status != "success" {
		t.Fatal(run)
	}
	watch, _ := store.Watch(context.Background(), 1)
	monitor.Close()
	due := time.Now().Add(-48 * time.Hour).UTC()
	watch.NextRunAt = &due
	if err := store.SaveWatch(context.Background(), &watch); err != nil {
		t.Fatal(err)
	}
	stale := repository.Run{WatchID: 1, UID: 2001, Source: "automatic", Status: "running", StartedAt: due}
	if err := store.SaveRun(context.Background(), &stale); err != nil {
		t.Fatal(err)
	}
	if err := store.Close(context.Background()); err != nil {
		t.Fatal(err)
	}
	_, reopened, restarted := openAppWithTransport(t, cfg, io.Discard, f)
	interrupted, _ := reopened.Run(context.Background(), stale.ID)
	restored, _ := reopened.Watch(context.Background(), 1)
	if interrupted.Status != "interrupted" || restored.TopicCursor != watch.TopicCursor || restored.ReplyCursor != watch.ReplyCursor || !restored.BaselineComplete {
		t.Fatalf("restart lost committed UID progress: %+v %+v", interrupted, restored)
	}
	// 实际循环启动，过期两天也只跑一轮，不按离线间隔补造任务。
	restarted.StartScheduler()
	for deadline := time.Now().Add(6 * time.Second); time.Now().Before(deadline); {
		runs, _ := reopened.WatchRuns(context.Background(), 1)
		if len(runs) == 3 && runs[0].Status == "success" {
			break
		}
		time.Sleep(30 * time.Millisecond)
	}
	restarted.Close()
	runs, _ := reopened.WatchRuns(context.Background(), 1)
	if len(runs) != 3 || runs[0].Source != "automatic" || runs[0].Status != "success" || runs[0].Silent || runs[0].Saved != 0 {
		t.Fatalf("restart did not resume once from watermarks: %+v", runs)
	}
	if err := reopened.Close(context.Background()); err != nil {
		t.Fatal(err)
	}
	cfg.BackgroundEnabled = false
	_, verified, disabled := openAppWithTransport(t, cfg, io.Discard, f)
	f.mu.Lock()
	calls := len(f.calls)
	f.mu.Unlock()
	disabled.StartScheduler()
	if err := disabled.Tick(context.Background(), time.Now().Add(48*time.Hour)); err != nil {
		t.Fatal(err)
	}
	verifiedRuns, _ := verified.WatchRuns(context.Background(), 1)
	f.mu.Lock()
	after := len(f.calls)
	f.mu.Unlock()
	if calls != after || len(verifiedRuns) != len(runs) {
		t.Fatal("verification mode started background collection")
	}
}

func TestScheduleInputValidation(t *testing.T) {
	router, _, _ := openAppWithTransport(t, testConfig(t), io.Discard, nil)
	for _, value := range []int{0, 29, 86401} {
		response := send(t, router, "POST", "/admin/watches", "", url.Values{"tid": {"1001"}, "init_mode": {"full"}, "interval_seconds": {fmt.Sprint(value)}})
		expectStatus(t, response, 400)
	}
	expectStatus(t, send(t, router, "POST", "/admin/watches", "", url.Values{"kind": {"uid"}, "uid": {"2001"}, "interval_seconds": {"60"}, "schedule_present": {"1"}, "period_weekdays": {"1"}, "period_start": {"24:00"}, "period_end": {"03:00"}}), 400)
}
