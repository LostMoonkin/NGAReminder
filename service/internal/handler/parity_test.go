package handler

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"reflect"
	"strings"
	"testing"
	"time"

	"ngareminder/service/internal/infrastructure"
	"ngareminder/service/internal/repository"
	"ngareminder/service/internal/service"
)

func TestUIDMissingPostRemainsRetryable(t *testing.T) {
	cfg := testConfig(t)
	cfg.BackgroundEnabled = true
	f := userNGA(t)
	router, store, m := openAppWithTransport(t, cfg, io.Discard, f)
	ctx := context.Background()
	expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, service.AccountInput{PassportUID: "2009", PassportCID: "fixture-valid-secret"}), 200)
	response := send(t, router, "POST", "/api/v1/watches", cfg.APIToken, service.WatchInput{Kind: "uid", UID: 2001})
	expectStatus(t, response, 201)
	watch := payload[repository.Watch](t, response)
	if run := runWatch(t, router, cfg.APIToken, watch.ID); run.Status != "success" {
		t.Fatal(run)
	}
	before, _ := store.Watch(ctx, watch.ID)
	f.mu.Lock()
	f.live, f.failPID, f.failCode = true, "4004", 14
	f.mu.Unlock()
	run := runWatch(t, router, cfg.APIToken, watch.ID)
	after, _ := store.Watch(ctx, watch.ID)
	if run.Status != "failed" || after.State != "ready" || before.TopicCursor != after.TopicCursor || before.ReplyCursor != after.ReplyCursor {
		t.Fatal("missing candidate stopped UID or advanced cursor", run, after)
	}
	posts, total, err := store.Posts(ctx, 1003, 1)
	if err != nil || total != 0 || len(posts) != 0 {
		t.Fatal("failed UID batch committed partial content", err)
	}
	f.mu.Lock()
	f.failPID = ""
	f.mu.Unlock()
	if err = m.Tick(ctx, time.Now().Add(time.Hour)); err != nil {
		t.Fatal(err)
	}
	runs, err := store.WatchRuns(ctx, watch.ID)
	if err != nil || len(runs) != 3 || runs[0].Source != "automatic" {
		t.Fatal("UID did not retry automatically", runs, err)
	}
	deadline := time.Now().Add(15 * time.Second)
	for time.Now().Before(deadline) {
		done, _ := store.Run(ctx, runs[0].ID)
		if done.Status != "running" {
			if done.Status != "success" {
				t.Fatal(done)
			}
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatal("automatic retry did not finish")
}

func TestSavedMetadataUsersRawAndLinks(t *testing.T) {
	for _, rawEnabled := range []bool{false, true} {
		t.Run(fmt.Sprint(rawEnabled), func(t *testing.T) {
			cfg := testConfig(t)
			cfg.BackgroundEnabled, cfg.StoreRawPayload = true, rawEnabled
			f := userNGA(t)
			router, store, _ := openAppWithTransport(t, cfg, io.Discard, f)
			ctx := context.Background()
			expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, service.AccountInput{PassportUID: "2009", PassportCID: "fixture-valid-secret"}), 200)
			expectStatus(t, send(t, router, "POST", "/api/v1/watches", cfg.APIToken, service.WatchInput{Kind: "uid", UID: 2001}), 201)
			if run := runWatch(t, router, cfg.APIToken, 1); run.Status != "success" {
				t.Fatal(run)
			}
			f.mu.Lock()
			f.live = true
			f.mu.Unlock()
			if run := runWatch(t, router, cfg.APIToken, 1); run.Status != "success" {
				t.Fatal(run)
			}
			thread, err := store.Thread(ctx, 1003)
			if err != nil || thread.Coverage != "partial" || thread.Title == "" || thread.FID != 3001 || thread.RemoteRows != 0 {
				t.Fatal("reply-only thread metadata missing", thread, err)
			}
			posts, total, err := store.Posts(ctx, 1003, 1)
			if err != nil || total != 2 {
				t.Fatal(total, err)
			}
			for _, p := range posts {
				if p.PageNumber != 1 || (len(p.RawPayload) > 0) != rawEnabled {
					t.Fatal("page/raw persistence mismatch", p)
				}
				if rawEnabled && !json.Valid(p.RawPayload) {
					t.Fatal("invalid saved raw")
				}
				if strings.Contains(p.SourceURL, "page=") || !strings.Contains(p.WebURL(), "#pid") {
					t.Fatal("notification and web links mixed")
				}
			}
			empty := repository.Watch{Kind: "uid", UID: 9000, Title: "Empty user", Paused: true, State: "ready"}
			if err = store.SaveWatch(ctx, &empty); err != nil {
				t.Fatal(err)
			}
			response := send(t, router, "GET", "/api/v1/users", cfg.APIToken, nil)
			expectStatus(t, response, 200)
			summary := payload[struct {
				Users []repository.UserSummary `json:"users"`
			}](t, response)
			if len(summary.Users) != 2 || summary.Users[0].UID != 2001 || summary.Users[0].PostCount != 3 || summary.Users[0].ThreadCount != 2 || summary.Users[0].LastPublishedAt == nil || summary.Users[1].PostCount != 0 {
				t.Fatal("user aggregates incomplete", response.Body.String())
			}
			expectStatus(t, send(t, router, "GET", "/admin/users", "", nil), 200)
			response = send(t, router, "GET", "/admin/users/2001", "", nil)
			expectStatus(t, response, 200)
			if !strings.Contains(response.Body.String(), "#pid4003Anchor") {
				t.Fatal("web anchor missing")
			}
			full := thread
			full.Coverage, full.Title, full.RemoteRows, full.RemoteTotalPages = "full", "Full title", 100, 5
			if err = store.UpsertThread(ctx, full); err != nil {
				t.Fatal(err)
			}
			if err = store.UpsertThread(ctx, repository.Thread{TID: 1003, Coverage: "partial", Title: "Reply title"}); err != nil {
				t.Fatal(err)
			}
			saved, _ := store.Thread(ctx, 1003)
			if saved.Title != "Full title" || saved.RemoteRows != 100 || saved.Coverage != "full" {
				t.Fatal("partial metadata overwrote full", saved)
			}
			if err = store.UpsertThread(ctx, repository.Thread{TID: 9090, Title: "No posts", Coverage: "full"}); err != nil {
				t.Fatal(err)
			}
			threads, _ := store.Threads(ctx)
			if threads[0].TID != 9090 || threads[0].Count != 0 {
				t.Fatal("empty thread missing or order incorrect", threads)
			}
		})
	}
}

func TestRenewalWebTestAndCancel(t *testing.T) {
	f := newLoginFixture(t)
	router, store, m, token := setupRenewal(t, f, io.Discard)
	ctx := context.Background()
	before, _ := store.Account(ctx)
	f.mu.Lock()
	messages := len(f.notices)
	f.mu.Unlock()
	response := send(t, router, "POST", "/api/v1/renewal/test", token, nil)
	expectStatus(t, response, 200)
	if !payload[service.RenewalTestResult](t, response).OK {
		t.Fatal("protocol probe failed")
	}
	f.mu.Lock()
	prepared, submitted, afterMessages := f.prepared, f.submitted, len(f.notices)
	f.mu.Unlock()
	after, _ := store.Account(ctx)
	current, _ := store.LatestRenewal(ctx)
	if prepared != 1 || submitted != 0 || messages != afterMessages || !reflect.DeepEqual(before, after) || current.ID != "" {
		t.Fatal("probe submitted credentials or changed account/session")
	}
	request, err := m.Renewal().Start(ctx)
	if err != nil {
		t.Fatal(err)
	}
	expectStatus(t, send(t, router, "POST", "/api/v1/renewal/cancel", token, nil), 204)
	expectStatus(t, send(t, router, "POST", "/api/v1/renewal/cancel", token, nil), 204)
	current, _ = store.LatestRenewal(ctx)
	if current.ID != request.ID || current.Status != "cancelled" || len(current.Expected) != 0 {
		t.Fatal(current)
	}
	if _, err = loginCommand(m, "late-confirm", "owner", "private", "p2p", "confirm", request.ID); err != nil {
		t.Fatal(err)
	}
	f.mu.Lock()
	prepared = f.prepared
	f.mu.Unlock()
	if prepared != 1 {
		t.Fatal("cancelled session prepared login")
	}
	expectStatus(t, send(t, router, "POST", "/admin/renewal/test", "", nil), 200)
	expectStatus(t, send(t, router, "POST", "/admin/renewal/cancel", "", nil), 303)
	expectStatus(t, send(t, router, "POST", "/api/v1/renewal/test", "", nil), 401)
}

func TestAuthenticationAlertsWithoutRenewal(t *testing.T) {
	cfg := testConfig(t)
	cfg.BackgroundEnabled = true
	f := &noticeFixture{nga: fixtureNGA(t)}
	router, store, m := openAppWithTransport(t, cfg, io.Discard, f)
	ctx := context.Background()
	valid := service.AccountInput{PassportUID: "2001", PassportCID: "fixture-valid-secret"}
	expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, valid), 200)
	if _, err := m.Notifications().SaveApp(ctx, infrastructure.AppCredentials{AppID: "cli_alert", AppSecret: "fixture-app-secret"}); err != nil {
		t.Fatal(err)
	}
	for _, input := range []service.ChannelInput{
		{Name: "Bark", Kind: "bark", Enabled: true, ChannelTarget: infrastructure.ChannelTarget{ServerURL: "https://bark.example.invalid", DeviceKey: "fixture"}},
		{Name: "Feishu", Kind: "feishu", Enabled: true, ChannelTarget: infrastructure.ChannelTarget{ReceiveID: "fixture-chat", ReceiveIDType: "chat_id"}},
		{Name: "Disabled", Kind: "bark", Enabled: false, ChannelTarget: infrastructure.ChannelTarget{ServerURL: "https://bark.example.invalid", DeviceKey: "fixture-disabled"}},
	} {
		if _, err := m.Notifications().SaveChannel(ctx, 0, input); err != nil {
			t.Fatal(err)
		}
	}
	watch := createWatch(t, router, cfg.APIToken, 1001, "full")
	f.nga.mu.Lock()
	f.nga.authFail = true
	f.nga.mu.Unlock()
	runWatch(t, router, cfg.APIToken, watch.ID)
	alerts, err := store.Alerts(ctx, 1)
	if err != nil || len(alerts) != 1 || len(alerts[0].Deliveries) != 2 {
		t.Fatal("auth alert did not reach every channel", alerts, err)
	}
	expectStatus(t, send(t, router, "POST", "/api/v1/nga-account/check", cfg.APIToken, nil), 422)
	alerts, _ = store.Alerts(ctx, 1)
	if len(alerts) != 1 {
		t.Fatal("duplicate auth alert")
	}
	f.mu.Lock()
	f.barkFail = true
	f.mu.Unlock()
	now := time.Now().Add(time.Hour)
	if err = m.Notifications().Deliver(ctx, now); err != nil {
		t.Fatal(err)
	}
	alerts, _ = store.Alerts(ctx, 1)
	if alerts[0].Deliveries[0].Attempts != 1 || alerts[0].Deliveries[0].Status != "pending" {
		t.Fatal("alert retry not persisted")
	}
	f.mu.Lock()
	f.barkFail = false
	f.mu.Unlock()
	for i := 0; i < 2; i++ {
		if err = m.Notifications().Deliver(ctx, now.Add(time.Hour)); err != nil {
			t.Fatal(err)
		}
	}
	alerts, _ = store.Alerts(ctx, 1)
	for _, d := range alerts[0].Deliveries {
		if d.Status != "sent" {
			t.Fatal(d)
		}
	}
	f.nga.mu.Lock()
	f.nga.authFail = false
	f.nga.mu.Unlock()
	expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, valid), 200)
	alerts, _ = store.Alerts(ctx, 1)
	if alerts[0].ResolvedAt == nil {
		t.Fatal("alert did not resolve")
	}
	expectStatus(t, send(t, router, "GET", "/admin/notifications", "", nil), 200)
	f.nga.mu.Lock()
	f.nga.authFail = true
	f.nga.mu.Unlock()
	runWatch(t, router, cfg.APIToken, watch.ID)
	alerts, _ = store.Alerts(ctx, 1)
	if len(alerts) != 2 || len(alerts[0].Deliveries) != 2 {
		t.Fatal("new invalidation did not requeue", alerts)
	}
}

func TestCancelRenewalDuringNetworkWork(t *testing.T) {
	for _, phase := range []string{"prepare", "validate"} {
		t.Run(phase, func(t *testing.T) {
			f := newLoginFixture(t)
			router, store, m, token := setupRenewal(t, f, io.Discard)
			ctx := context.Background()
			before, _ := store.Account(ctx)
			request, err := m.Renewal().Start(ctx)
			if err != nil {
				t.Fatal(err)
			}
			action, args := "confirm", []string{"confirm", request.ID}
			if phase == "validate" {
				if _, err = loginCommand(m, "confirm", "owner", "private", "p2p", args...); err != nil {
					t.Fatal(err)
				}
				action, args = "captcha", []string{"captcha", request.ID, "aB1234"}
			}
			f.mu.Lock()
			f.blockPhase, f.entered = phase, make(chan struct{})
			entered := f.entered
			f.mu.Unlock()
			done := make(chan struct{})
			go func() {
				defer close(done)
				_, _ = loginCommand(m, action+"-blocked", "owner", "private", "p2p", args...)
			}()
			select {
			case <-entered:
			case <-time.After(5 * time.Second):
				t.Fatal("network operation did not begin")
			}
			expectStatus(t, send(t, router, "POST", "/api/v1/renewal/cancel", token, nil), 204)
			select {
			case <-done:
			case <-time.After(5 * time.Second):
				t.Fatal("cancel did not interrupt network request")
			}
			after, _ := store.Account(ctx)
			current, _ := store.LatestRenewal(ctx)
			if !reflect.DeepEqual(before, after) || current.Status != "cancelled" || len(current.Expected) > 0 {
				t.Fatal("cancel committed candidate or retained context", current)
			}
		})
	}
}

func TestNotificationTitlesFollowRust(t *testing.T) {
	cfg := testConfig(t)
	cfg.BackgroundEnabled = true
	f := &noticeFixture{nga: fixtureNGA(t)}
	_, store, m := openAppWithTransport(t, cfg, io.Discard, f)
	ctx := context.Background()
	channel, err := m.Notifications().SaveChannel(ctx, 0, service.ChannelInput{Name: "Bark", Kind: "bark", Enabled: true, ChannelTarget: infrastructure.ChannelTarget{ServerURL: "https://bark.example.invalid", DeviceKey: "fixture"}})
	if err != nil {
		t.Fatal(err)
	}
	for index, kind := range []string{"tid", "uid", "uid"} {
		author, want := "Nickname", "Stored thread title"
		if kind == "uid" {
			want = "用户监控：Nickname"
		}
		if index == 2 {
			author, want = "", "用户监控：UID 2001"
		}
		post := repository.Post{TID: 1001, PID: int64(4001 + index), Key: fmt.Sprintf("pid:%d", 4001+index), Kind: "reply", Floor: 7, AuthorUID: 2001, Author: author, Subject: "Misleading reply subject", Body: "Body", SourceURL: fmt.Sprintf("https://bbs.nga.cn/read.php?tid=1001&pid=%d", 4001+index)}
		watch := repository.Watch{ID: int64(index + 1), Kind: kind, UID: 2001, Title: "Different profile name", ChannelIDs: []int64{channel.ID}}
		if err = store.Transaction(ctx, func(ctx context.Context, tx *repository.Store) error {
			if err := tx.UpsertThread(ctx, repository.Thread{TID: 1001, Title: "Stored thread title", Coverage: "full"}); err != nil {
				return err
			}
			if _, err := tx.InsertPosts(ctx, []repository.Post{post}); err != nil {
				return err
			}
			return m.Notifications().Record(ctx, tx, watch, []repository.Post{post}, false)
		}); err != nil {
			t.Fatal(err)
		}
		if err = m.Notifications().Deliver(ctx, time.Now().Add(time.Hour)); err != nil {
			t.Fatal(err)
		}
		f.mu.Lock()
		message := f.message
		f.mu.Unlock()
		var notice map[string]string
		if err = json.Unmarshal([]byte(message), &notice); err != nil {
			t.Fatal(err)
		}
		if notice["title"] != want || !strings.Contains(notice["body"], " · #7") || notice["url"] != post.SourceURL {
			t.Fatal("notice differs from Rust", notice, want)
		}
	}
}
