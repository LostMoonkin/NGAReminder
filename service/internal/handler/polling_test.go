package handler

import (
	"bytes"
	"context"
	"strings"
	"testing"
	"time"

	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/repository"
)

func TestIdlePollingDoesNotLog(t *testing.T) {
	cfg := testConfig(t)
	cfg.BackgroundEnabled = true
	var logs bytes.Buffer
	_, store, monitor := openAppWithTransport(t, cfg, &logs, fixtureNGA(t))
	ctx := logging.New(&logs).WithContext(context.Background())
	now := time.Now().UTC()
	logs.Reset()
	for _, poll := range []func(context.Context, time.Time) error{monitor.Tick, monitor.Notifications().Deliver, monitor.Renewal().Expire} {
		if err := poll(ctx, now); err != nil {
			t.Fatal(err)
		}
	}
	if logs.Len() != 0 {
		t.Fatalf("empty polling emitted logs: %s", logs.String())
	}

	// 有记录但没有可执行任务时，也不能恢复轮询日志。
	future := now.Add(time.Hour)
	if err := store.SaveAccount(ctx, &repository.Account{ID: 1, Status: "valid"}); err != nil {
		t.Fatal(err)
	}
	for _, watch := range []repository.Watch{
		{TID: 1001, State: "ready", NextRunAt: &future},
		{TID: 1002, State: "ready", Paused: true},
		{TID: 1003, State: "auth_paused"},
		{TID: 1004, State: "ready", NoFetchPeriods: []repository.TimeWindow{{Weekdays: []int{1, 2, 3, 4, 5, 6, 7}, Start: "00:00", End: "00:00"}}},
	} {
		if err := store.SaveWatch(ctx, &watch); err != nil {
			t.Fatal(err)
		}
	}
	channel := repository.Channel{Name: "Disabled", Kind: "bark"}
	if err := store.SaveChannel(ctx, &channel); err != nil {
		t.Fatal(err)
	}
	for _, delivery := range []repository.Delivery{
		{EventID: 1, ChannelID: channel.ID, Status: "pending", NextAttempt: now},
		{EventID: 2, ChannelID: channel.ID, Status: "pending", NextAttempt: future},
	} {
		if err := store.SaveDelivery(ctx, &delivery); err != nil {
			t.Fatal(err)
		}
	}
	if err := store.SaveRenewal(ctx, &repository.RenewalRequest{ID: "waiting", Status: "awaiting_confirmation", ExpiresAt: future}); err != nil {
		t.Fatal(err)
	}
	if err := store.SaveFloorGap(ctx, &repository.FloorGap{WatchID: 1, Floor: 2, Status: "pending", FirstSeen: now, Deadline: future}); err != nil {
		t.Fatal(err)
	}
	logs.Reset()
	monitor.StartScheduler()
	time.Sleep(4200 * time.Millisecond)
	monitor.Close()
	for _, event := range events(t, logs.Bytes()) {
		if event["operation"] != "service.stop_monitoring" {
			t.Fatalf("idle background loop emitted a log: %+v", event)
		}
	}
}

func TestPollingLogsStateChanges(t *testing.T) {
	cfg := testConfig(t)
	cfg.BackgroundEnabled = true
	var logs bytes.Buffer
	_, store, monitor := openAppWithTransport(t, cfg, &logs, fixtureNGA(t))
	ctx := logging.New(&logs).WithContext(context.Background())
	now := time.Now().UTC()
	if err := store.SaveRenewal(ctx, &repository.RenewalRequest{ID: "expired", Status: "awaiting_confirmation", ExpiresAt: now}); err != nil {
		t.Fatal(err)
	}
	if err := store.SaveFloorGap(ctx, &repository.FloorGap{WatchID: 1, Floor: 2, Status: "pending", FirstSeen: now.Add(-3 * time.Hour), Deadline: now.Add(-time.Second)}); err != nil {
		t.Fatal(err)
	}
	logs.Reset()
	if err := monitor.Renewal().Expire(ctx, now); err != nil {
		t.Fatal(err)
	}
	if err := monitor.Tick(ctx, now); err != nil {
		t.Fatal(err)
	}
	starts := map[string]bool{}
	sql := map[string]bool{}
	for _, event := range events(t, logs.Bytes()) {
		traceID, _ := event["trace_id"].(string)
		if traceID == "" {
			t.Fatal("business log is missing its trace ID")
		}
		if event["event"] == "start" {
			starts[event["span_id"].(string)] = true
		}
		if event["event"] == "sql" {
			sql[traceID] = true
		}
		if parent, _ := event["parent_span_id"].(string); parent != "" && !starts[parent] {
			t.Fatalf("business span is missing its parent: %+v", event)
		}
	}
	if len(sql) != 2 || !strings.Contains(logs.String(), "Expiring renewal") || !strings.Contains(logs.String(), "Expiring floor gap") {
		t.Fatal("state changes must retain business parameters and database traces")
	}
	renewal, err := store.LatestRenewal(ctx)
	if err != nil || renewal.Status != "expired" {
		t.Fatal("renewal did not expire", renewal, err)
	}
	gaps, err := store.FloorGaps(ctx, 1)
	if err != nil || len(gaps) != 1 || gaps[0].Status != "expired" {
		t.Fatal("floor gap did not expire", gaps, err)
	}
	logs.Reset()
	if err := monitor.Renewal().Expire(ctx, now.Add(time.Second)); err != nil {
		t.Fatal(err)
	}
	if err := monitor.Tick(ctx, now.Add(time.Second)); err != nil {
		t.Fatal(err)
	}
	if logs.Len() != 0 {
		t.Fatal("completed state changes must not log again on the next tick")
	}
}

func TestPollingReadErrorsRemainVisible(t *testing.T) {
	cfg := testConfig(t)
	cfg.BackgroundEnabled = true
	var logs bytes.Buffer
	_, store, monitor := openAppWithTransport(t, cfg, &logs, fixtureNGA(t))
	ctx := logging.New(&logs).WithContext(context.Background())
	if err := store.Close(ctx); err != nil {
		t.Fatal(err)
	}
	for _, poll := range []func(context.Context, time.Time) error{monitor.Tick, monitor.Notifications().Deliver, monitor.Renewal().Expire} {
		logs.Reset()
		if err := poll(ctx, time.Now()); err == nil {
			t.Fatal("closed database must fail the poll")
		}
		count := 0
		for _, event := range events(t, logs.Bytes()) {
			if event["level"] == "error" {
				count++
				if event["trace_id"] == "" || event["error"] == nil || len(event["causes"].([]any)) == 0 || len(event["stack"].([]any)) == 0 {
					t.Fatal("polling error is missing its trace, cause, or stack")
				}
			}
		}
		if count != 1 {
			t.Fatalf("polling error was logged %d times, want 1", count)
		}
	}
}
