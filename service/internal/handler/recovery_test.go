package handler

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"ngareminder/service/internal/repository"
	"ngareminder/service/internal/service"
	"reflect"
	"testing"
	"time"
)

type historyFixture struct {
	*userFixture
	start    int64
	blockPID string
	block    chan struct{}
}

func (f *historyFixture) RoundTrip(r *http.Request) (*http.Response, error) {
	if err := r.ParseForm(); err != nil {
		return nil, err
	}
	if r.URL.Path == "/thread.php" && r.Form.Get("authorid") == "2001" {
		if r.Form.Get("searchpost") != "1" {
			return nil, errors.New("backfill requested historical topics")
		}
		f.mu.Lock()
		f.calls = append(f.calls, userRequest{path: r.URL.Path, page: r.Form.Get("page"), uid: "2001"})
		f.mu.Unlock()
		items := []any{}
		values := [][2]int64{{4900, time.Now().Add(time.Hour).Unix()}, {4003, f.start + 100}, {4005, f.start + 99}}
		if r.Form.Get("page") == "2" {
			values = [][2]int64{{4004, f.start}, {4006, f.start - 1}}
		}
		for _, v := range values {
			items = append(items, map[string]any{"__P": map[string]any{"tid": 1003, "pid": v[0], "authorid": 2001, "postdate": v[1]}})
		}
		raw, _ := json.Marshal(map[string]any{"code": 0, "result": map[string]any{"__T": items, "__ROWS": 5, "__R__ROWS_PAGE": 3}})
		return fixtureResponse(string(raw)), nil
	}
	f.mu.Lock()
	blockPID, block := f.blockPID, f.block
	f.mu.Unlock()
	if r.Form.Get("pid") == blockPID && block != nil {
		select {
		case <-block:
		case <-r.Context().Done():
			return nil, r.Context().Err()
		}
	}
	out, err := f.userFixture.RoundTrip(r)
	if err != nil {
		return out, err
	}
	if r.URL.Path == "/app_api.php" && r.Form.Get("pid") != "" {
		raw, _ := io.ReadAll(out.Body)
		_ = out.Body.Close()
		var value map[string]any
		_ = json.Unmarshal(raw, &value)
		if items, ok := value["result"].([]any); ok && len(items) > 0 {
			post := items[0].(map[string]any)
			stamp := f.start + 100
			if r.Form.Get("pid") == "4004" {
				stamp = f.start
			}
			post["postdatetimestamp"] = stamp
			raw, _ = json.Marshal(value)
		}
		return fixtureResponse(string(raw)), nil
	}
	return out, nil
}
func startHistory(t *testing.T, router http.Handler, token string, watchID int64, date string) repository.Backfill {
	t.Helper()
	for deadline := time.Now().Add(5 * time.Second); time.Now().Before(deadline); {
		response := send(t, router, "POST", fmt.Sprintf("/api/v1/watches/%d/backfills", watchID), token, map[string]string{"start_date": date})
		if response.Code == 409 {
			time.Sleep(10 * time.Millisecond)
			continue
		}
		expectStatus(t, response, 202)
		return payload[repository.Backfill](t, response)
	}
	t.Fatal("backfill was not accepted")
	return repository.Backfill{}
}
func awaitBackfill(t *testing.T, store *repository.Store, id int64) repository.Backfill {
	t.Helper()
	for deadline := time.Now().Add(10 * time.Second); time.Now().Before(deadline); {
		v, err := store.Backfill(context.Background(), id)
		if err != nil {
			t.Fatal(err)
		}
		if v.Status != "running" {
			return v
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatal("backfill did not finish")
	return repository.Backfill{}
}
func TestHistoryBackfillBoundariesSilenceAndInterruption(t *testing.T) {
	cfg := testConfig(t)
	cfg.BackgroundEnabled = true
	location, _ := time.LoadLocation(cfg.Timezone)
	start, _ := time.ParseInLocation("2006-01-02", "2026-01-01", location)
	f := &historyFixture{userFixture: userNGA(t), start: start.Unix()}
	router, store, m := openAppWithTransport(t, cfg, io.Discard, f)
	ctx := context.Background()
	expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, service.AccountInput{PassportUID: "2009", PassportCID: "fixture-valid-secret"}), 200)
	response := send(t, router, "POST", "/api/v1/watches", cfg.APIToken, service.WatchInput{Kind: "uid", UID: 2001})
	expectStatus(t, response, 201)
	watch := payload[repository.Watch](t, response)
	watch.BaselineComplete = true
	watch.TopicCursor = repository.UserCursor{Timestamp: 100, ID: 99}
	watch.ReplyCursor = repository.UserCursor{Timestamp: 200, ID: 77}
	watch.Paused = true
	watch.NoFetchPeriods = []repository.TimeWindow{{Weekdays: []int{1, 2, 3, 4, 5, 6, 7}, Start: "00:00", End: "00:00"}}
	if err := store.SaveWatch(ctx, &watch); err != nil {
		t.Fatal(err)
	}
	before, _ := store.Watch(ctx, watch.ID)
	expectStatus(t, send(t, router, "POST", fmt.Sprintf("/api/v1/watches/%d/backfills", watch.ID), cfg.APIToken, map[string]string{"start_date": "2999-01-01"}), 400)
	tid := createWatch(t, router, cfg.APIToken, 9999, "full")
	expectStatus(t, send(t, router, "POST", fmt.Sprintf("/api/v1/watches/%d/backfills", tid.ID), cfg.APIToken, map[string]string{"start_date": "2026-01-01"}), 400)
	// 第二页详情失败，第一条已提交内容保留；重新提交补齐日期零点的回复。
	f.mu.Lock()
	f.failPID = "4004"
	f.mu.Unlock()
	job := startHistory(t, router, cfg.APIToken, watch.ID, "2026-01-01")
	failed := awaitBackfill(t, store, job.ID)
	if failed.Status != "failed" || failed.Saved != 1 {
		t.Fatal("failed backfill did not retain committed progress", failed)
	}
	f.mu.Lock()
	f.failPID = ""
	f.mu.Unlock()
	job = startHistory(t, router, cfg.APIToken, watch.ID, "2026-01-01")
	finished := awaitBackfill(t, store, job.ID)
	if finished.Status != "success" || finished.Saved != 1 || !finished.StartAt.Equal(start) {
		t.Fatal("date boundary or resumed collection failed", finished)
	}
	posts, total, err := store.Posts(ctx, 1003, 1)
	if err != nil || total != 2 {
		t.Fatal("unexpected historical content", total, err)
	}
	ids := map[int64]bool{}
	for _, post := range posts {
		ids[post.PID] = true
		if post.Kind != "reply" || post.Key == "main" || post.AuthorUID != 2001 {
			t.Fatal("by-PID reply identity was lost", post)
		}
	}
	if !ids[4003] || !ids[4004] {
		t.Fatal("same-TID replies collided or midnight was excluded", ids)
	}
	job = startHistory(t, router, cfg.APIToken, watch.ID, "2026-01-01")
	if done := awaitBackfill(t, store, job.ID); done.Status != "success" || done.Saved != 0 {
		t.Fatal("overlapping backfill duplicated content", done)
	}
	after, _ := store.Watch(ctx, watch.ID)
	if !reflect.DeepEqual(before, after) {
		t.Fatal("backfill changed real-time watch configuration", before, after)
	}
	inbox, _ := store.Inbox(ctx, 1)
	runs, _ := store.WatchRuns(ctx, watch.ID)
	if len(inbox) != 0 || len(runs) != 0 {
		t.Fatal("silent backfill created notifications or real-time runs")
	}
	expectStatus(t, request(t, router, "GET", fmt.Sprintf("/admin/watches/%d", watch.ID), ""), 200)
	// 关闭进程取消正在等待的第二页；重启显示中断并允许显式重跑。
	f.mu.Lock()
	f.blockPID = "4004"
	f.block = make(chan struct{})
	f.mu.Unlock()
	job = startHistory(t, router, cfg.APIToken, watch.ID, "2026-01-01")
	expectStatus(t, send(t, router, "POST", fmt.Sprintf("/api/v1/watches/%d/backfills", watch.ID), cfg.APIToken, map[string]string{"start_date": "2026-01-01"}), 409)
	m.Close()
	interrupted, _ := store.Backfill(ctx, job.ID)
	if interrupted.Status != "interrupted" {
		t.Fatal("shutdown did not record interruption", interrupted)
	}
	f.mu.Lock()
	f.block = nil
	f.blockPID = ""
	f.mu.Unlock()
	// 模拟异常退出留下的 running，再走实际数据库重开/初始化路径。
	stale := repository.Backfill{WatchID: watch.ID, UID: watch.UID, StartAt: start.UTC(), EndAt: time.Now().UTC(), Status: "running", Saved: 2}
	if err = store.SaveBackfill(ctx, &stale); err != nil {
		t.Fatal(err)
	}
	if err = store.Close(ctx); err != nil {
		t.Fatal(err)
	}
	reopenedRouter, reopened, _ := openAppWithTransport(t, cfg, io.Discard, f)
	stale, _ = reopened.Backfill(ctx, stale.ID)
	if stale.Status != "interrupted" || stale.Saved != 2 {
		t.Fatal("restart lost unfinished backfill progress")
	}
	job = startHistory(t, reopenedRouter, cfg.APIToken, watch.ID, "2026-01-01")
	if done := awaitBackfill(t, reopened, job.ID); done.Status != "success" || done.Saved != 0 {
		t.Fatal("restart could not rerun the same range", done)
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	for _, call := range f.calls {
		if call.path == "/app_api.php" && (call.pid == "" || call.pid == "4006" || call.pid == "4900") {
			t.Fatal("backfill fetched a whole topic or out-of-range reply", call)
		}
	}
}
func fixtureReply(floor, pid, uid int64) map[string]any {
	return map[string]any{"tid": 1001, "pid": pid, "lou": floor, "postdatetimestamp": 1767225900, "content": "recovered fixture", "author": map[string]any{"uid": uid, "username": "fixture author"}}
}
func tickRun(t *testing.T, m *service.Monitoring, store *repository.Store, watchID int64, now time.Time) repository.Run {
	t.Helper()
	old, _ := store.WatchRuns(context.Background(), watchID)
	last := int64(0)
	if len(old) > 0 {
		last = old[0].ID
	}
	for deadline := time.Now().Add(5 * time.Second); time.Now().Before(deadline); {
		if err := m.Tick(context.Background(), now); err != nil {
			t.Fatal(err)
		}
		runs, _ := store.WatchRuns(context.Background(), watchID)
		if len(runs) > 0 && runs[0].ID > last {
			return awaitRun(t, store, watchID)
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatal("due gap run did not start")
	return repository.Run{}
}
func TestFloorGapDetectionRecoveryAndCurrentNotificationRules(t *testing.T) {
	cfg := testConfig(t)
	cfg.BackgroundEnabled = true
	f := fixtureNGA(t)
	router, store, m := openAppWithTransport(t, cfg, io.Discard, f)
	ctx := context.Background()
	expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, service.AccountInput{PassportUID: "2001", PassportCID: "fixture-valid-secret"}), 200)
	watch := createWatch(t, router, cfg.APIToken, 1001, "full")
	runWatch(t, router, cfg.APIToken, watch.ID)
	gaps, _ := store.FloorGaps(ctx, watch.ID)
	if len(gaps) != 0 {
		t.Fatal("baseline holes created gaps")
	}
	f.mu.Lock()
	f.page1["vrows"] = 99999
	f.mu.Unlock()
	runWatch(t, router, cfg.APIToken, watch.ID)
	gaps, _ = store.FloorGaps(ctx, watch.ID)
	if len(gaps) != 0 {
		t.Fatal("reply count change created gaps")
	}
	f.mu.Lock()
	f.newPosts = true
	f.page2["result"] = append(f.page2["result"].([]any), fixtureReply(11, 4011, 2005))
	f.mu.Unlock()
	runWatch(t, router, cfg.APIToken, watch.ID)
	gaps, _ = store.FloorGaps(ctx, watch.ID)
	if len(gaps) != 2 || gaps[0].Floor != 8 || gaps[1].Floor != 10 {
		t.Fatal("actual floor gaps were not recorded", gaps)
	}
	oldEvents, _ := store.Inbox(ctx, 1)
	channel := repository.Channel{Name: "current fixture channel", Kind: "bark", Enabled: true}
	if err := store.SaveChannel(ctx, &channel); err != nil {
		t.Fatal(err)
	}
	watch, _ = store.Watch(ctx, watch.ID)
	watch.AuthorUIDs = []int64{2005}
	watch.ChannelIDs = []int64{channel.ID}
	future := time.Now().Add(24 * time.Hour)
	watch.NextRunAt = &future
	if err := store.SaveWatch(ctx, &watch); err != nil {
		t.Fatal(err)
	}
	f.mu.Lock()
	f.page2["result"] = append(f.page2["result"].([]any), fixtureReply(8, 4008, 2005), fixtureReply(10, 4010, 9999), fixtureReply(6, 4006, 2005))
	beforeCalls := f.calls
	f.mu.Unlock()
	run := tickRun(t, m, store, watch.ID, gaps[0].FirstSeen.Add(5*time.Minute))
	if run.Source != "gap_recovery" || run.Status != "success" || run.Saved != 2 {
		t.Fatal("recovery did not save only recorded gaps", run)
	}
	gaps, _ = store.FloorGaps(ctx, watch.ID)
	for _, gap := range gaps {
		if gap.Status != "resolved" {
			t.Fatal("recovered gap stayed pending", gap)
		}
	}
	current, _ := store.Watch(ctx, watch.ID)
	if current.CursorFloor != 11 || !current.NextRunAt.Equal(future) {
		t.Fatal("gap recovery changed live cursor or schedule", current)
	}
	events, _ := store.Inbox(ctx, 1)
	if len(events) != len(oldEvents)+1 {
		t.Fatal("recovery ignored the current author filter", len(events), len(oldEvents))
	}
	found := false
	for _, event := range events {
		if event.Post.PID == 4008 {
			found = true
			if len(event.Deliveries) != 1 || event.Deliveries[0].ChannelID != channel.ID {
				t.Fatal("recovery ignored current channels")
			}
		}
	}
	if !found {
		t.Fatal("recovered content did not produce an event")
	}
	f.mu.Lock()
	afterCalls := f.calls
	f.mu.Unlock()
	if afterCalls-beforeCalls != 2 {
		t.Fatal("same-topic gaps were fetched more than once per page", afterCalls-beforeCalls)
	}
	if err := m.Tick(ctx, gaps[0].FirstSeen.Add(6*time.Minute)); err != nil {
		t.Fatal(err)
	}
	runWatch(t, router, cfg.APIToken, watch.ID)
	events, _ = store.Inbox(ctx, 1)
	if len(events) != len(oldEvents)+1 {
		t.Fatal("normal collection duplicated recovered notifications")
	}
	if _, err := store.PostByKey(ctx, 1001, "pid:4006"); !errors.Is(err, repository.ErrNotFound) {
		t.Fatal("unrecorded baseline hole was treated as an incremental recovery", err)
	}
}
func TestGapPausesDeadlineRestartAndInvalidation(t *testing.T) {
	cfg := testConfig(t)
	cfg.BackgroundEnabled = true
	f := fixtureNGA(t)
	router, store, m := openAppWithTransport(t, cfg, io.Discard, f)
	ctx := context.Background()
	expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, service.AccountInput{PassportUID: "2001", PassportCID: "fixture-valid-secret"}), 200)
	watch := createWatch(t, router, cfg.APIToken, 1001, "full")
	runWatch(t, router, cfg.APIToken, watch.ID)
	anchor := time.Now().UTC()
	gap := repository.FloorGap{WatchID: watch.ID, Floor: 8, Status: "pending", FirstSeen: anchor, Deadline: anchor.Add(120 * time.Minute)}
	if err := store.AddFloorGaps(ctx, []repository.FloorGap{gap}); err != nil {
		t.Fatal(err)
	}
	watch, _ = store.Watch(ctx, watch.ID)
	far := anchor.Add(24 * time.Hour)
	watch.NextRunAt = &far
	watch.Paused = true
	_ = store.SaveWatch(ctx, &watch)
	f.mu.Lock()
	calls := f.calls
	f.mu.Unlock()
	if err := m.Tick(ctx, anchor.Add(5*time.Minute)); err != nil {
		t.Fatal(err)
	}
	watch.Paused = false
	watch.NoFetchPeriods = []repository.TimeWindow{{Weekdays: []int{1, 2, 3, 4, 5, 6, 7}, Start: "00:00", End: "00:00"}}
	_ = store.SaveWatch(ctx, &watch)
	if err := m.Tick(ctx, anchor.Add(15*time.Minute)); err != nil {
		t.Fatal(err)
	}
	watch.NoFetchPeriods = nil
	watch.State = "auth_paused"
	_ = store.SaveWatch(ctx, &watch)
	if err := m.Tick(ctx, anchor.Add(20*time.Minute)); err != nil {
		t.Fatal(err)
	}
	f.mu.Lock()
	after := f.calls
	f.code = -1
	f.mu.Unlock()
	if after != calls {
		t.Fatal("pause/no-fetch/auth gate allowed a recovery request")
	}
	watch.State = "ready"
	_ = store.SaveWatch(ctx, &watch)
	run := tickRun(t, m, store, watch.ID, anchor.Add(36*time.Minute))
	if run.Status != "failed" {
		t.Fatal("failed recovery reported success")
	}
	gaps, _ := store.FloorGaps(ctx, watch.ID)
	if gaps[0].NextAttempt != 3 || !gaps[0].Deadline.Equal(gap.Deadline) {
		t.Fatal("missed attempts were not skipped or deadline was extended", gaps)
	}
	m.Close()
	if err := store.Close(ctx); err != nil {
		t.Fatal(err)
	}
	_, store, m = openAppWithTransport(t, cfg, io.Discard, f)
	gaps, _ = store.FloorGaps(ctx, watch.ID)
	if !gaps[0].FirstSeen.Equal(anchor) || !gaps[0].Deadline.Equal(gap.Deadline) {
		t.Fatal("restart recalculated the recovery deadline")
	}
	last := tickRun(t, m, store, watch.ID, anchor.Add(120*time.Minute))
	if last.Status != "failed" {
		t.Fatal("final recovery attempt did not execute")
	}
	gaps, _ = store.FloorGaps(ctx, watch.ID)
	if gaps[0].Status != "expired" || gaps[0].NextAttempt != 6 {
		t.Fatal("final attempt did not end active recovery", gaps)
	}
	f.mu.Lock()
	calls = f.calls
	f.mu.Unlock()
	if err := m.Tick(ctx, anchor.Add(121*time.Minute)); err != nil {
		t.Fatal(err)
	}
	gaps, _ = store.FloorGaps(ctx, watch.ID)
	if gaps[0].Status != "expired" {
		t.Fatal("expired gap remained active")
	}
	f.mu.Lock()
	after = f.calls
	f.mu.Unlock()
	if after != calls {
		t.Fatal("expired gap triggered an NGA request")
	}
	if _, err := m.ChangeWatch(ctx, watch.ID, "reset", "full"); err != nil {
		t.Fatal(err)
	}
	gaps, _ = store.FloorGaps(ctx, watch.ID)
	if len(gaps) != 0 {
		t.Fatal("reset retained old gaps")
	}
	if err := store.AddFloorGaps(ctx, []repository.FloorGap{gap}); err != nil {
		t.Fatal(err)
	}
	if _, err := m.ChangeWatch(ctx, watch.ID, "delete", ""); err != nil {
		t.Fatal(err)
	}
	gaps, _ = store.FloorGaps(ctx, watch.ID)
	if len(gaps) != 0 {
		t.Fatal("delete retained old gaps")
	}
}
