package handler

import (
	"bytes"
	"context"
	"fmt"
	"image"
	"image/png"
	"io"
	"net/http"
	"strings"
	"sync"
	"testing"
	"time"

	"ngareminder/service/internal/infrastructure"
	"ngareminder/service/internal/repository"
	"ngareminder/service/internal/service"
)

type noticeFixture struct {
	nga                    *ngaFixture
	mu                     sync.Mutex
	barkFail               bool
	barkCalls, feishuCalls int
	message                string
}

func noticeResponse(body string) *http.Response {
	response := fixtureResponse(body)
	response.Header.Set("Content-Type", "application/json")
	return response
}

func (f *noticeFixture) RoundTrip(r *http.Request) (*http.Response, error) {
	if r.URL.Host == "bbs.nga.cn" {
		return f.nga.RoundTrip(r)
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	switch {
	case r.URL.Host == "bark.example.invalid":
		f.barkCalls++
		body, _ := io.ReadAll(r.Body)
		f.message = string(body)
		if f.barkFail {
			return noticeResponse(`{"code":500}`), nil
		}
		return noticeResponse(`{"code":200}`), nil
	case strings.Contains(r.URL.Path, "tenant_access_token"):
		return noticeResponse(`{"code":0,"tenant_access_token":"fixture-tenant-secret","expire":7200}`), nil
	case strings.HasSuffix(r.URL.Path, "/images"):
		return noticeResponse(`{"code":123}`), nil
	case strings.HasSuffix(r.URL.Path, "/messages"):
		f.feishuCalls++
		body, _ := io.ReadAll(r.Body)
		f.message = string(body)
		return noticeResponse(`{"code":0,"data":{"message_id":"fixture"}}`), nil
	case r.URL.Host == "img.nga.cn":
		if r.URL.Path == "/image.png" {
			var data bytes.Buffer
			_ = png.Encode(&data, image.NewRGBA(image.Rect(0, 0, 1, 1)))
			return noticeResponse(data.String()), nil
		}
		response := noticeResponse("not found")
		response.StatusCode = http.StatusNotFound
		return response, nil
	}
	return nil, fmt.Errorf("unexpected notification fixture endpoint")
}
func TestNotificationsMatchingDeliveryAndRestart(t *testing.T) {
	cfg := testConfig(t)
	cfg.BackgroundEnabled = true
	var logs bytes.Buffer
	f := &noticeFixture{nga: fixtureNGA(t)}
	router, store, m := openAppWithTransport(t, cfg, &logs, f)
	ctx := context.Background()
	expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, service.AccountInput{PassportUID: "2001", PassportCID: "fixture-valid-secret"}), 200)
	response := send(t, router, "POST", "/api/v1/channels", cfg.APIToken, service.ChannelInput{Name: "Bark", Kind: "bark", Enabled: true, ChannelTarget: infrastructure.ChannelTarget{ServerURL: "https://bark.example.invalid", DeviceKey: "fixture-device-secret"}})
	expectStatus(t, response, 201)
	bark := payload[repository.Channel](t, response)
	expectStatus(t, send(t, router, "POST", "/api/v1/feishu-app", cfg.APIToken, infrastructure.AppCredentials{AppID: "cli_fixture", AppSecret: "fixture-app-secret"}), 200)
	response = send(t, router, "POST", "/api/v1/channels", cfg.APIToken, service.ChannelInput{Name: "Feishu", Kind: "feishu", Enabled: true, ChannelTarget: infrastructure.ChannelTarget{ReceiveID: "fixture-recipient-secret", ReceiveIDType: "chat_id"}})
	expectStatus(t, response, 201)
	feishu := payload[repository.Channel](t, response)
	response = send(t, router, "POST", "/api/v1/watches", cfg.APIToken, service.WatchInput{TID: 1001, InitMode: "full", ChannelIDs: []int64{bark.ID, feishu.ID}, AuthorUIDs: []int64{2005}})
	expectStatus(t, response, 201)
	watch := payload[repository.Watch](t, response)
	if run := runWatch(t, router, cfg.APIToken, watch.ID); run.Status != "success" {
		t.Fatal(run)
	}
	events, err := store.Inbox(ctx, 1)
	if err != nil || len(events) != 0 {
		t.Fatal("baseline produced events", events, err)
	}
	f.nga.mu.Lock()
	f.nga.newPosts = true
	f.nga.mu.Unlock()
	if run := runWatch(t, router, cfg.APIToken, watch.ID); run.Status != "success" {
		t.Fatal(run)
	}
	events, err = store.Inbox(ctx, 1)
	if err != nil || len(events) != 2 {
		t.Fatal("matching increment did not produce events", events, err)
	}
	for _, event := range events {
		if len(event.Deliveries) != 2 || event.Post.AuthorUID != 2005 {
			t.Fatal("incorrect matching", event)
		}
	}
	uidWatch := repository.Watch{Kind: "uid", UID: 2005, State: "ready", InitMode: "from_now", ChannelIDs: []int64{bark.ID, feishu.ID}}
	if err = store.SaveWatch(ctx, &uidWatch); err != nil {
		t.Fatal(err)
	}
	if err = store.Transaction(ctx, func(ctx context.Context, tx *repository.Store) error {
		return m.Notifications().Record(ctx, tx, uidWatch, []repository.Post{events[0].Post}, false)
	}); err != nil {
		t.Fatal(err)
	}
	merged, err := store.Event(ctx, events[0].ID)
	if err != nil || len(merged.Sources) != 2 || len(merged.Deliveries) != 2 {
		t.Fatal("cross-watch deduplication failed", merged, err)
	}
	expectStatus(t, send(t, router, "POST", fmt.Sprintf("/api/v1/channels/%d/delete", bark.ID), cfg.APIToken, nil), 400)
	expectStatus(t, send(t, router, "POST", fmt.Sprintf("/api/v1/channels/%d/disable", bark.ID), cfg.APIToken, nil), 200)
	now := time.Now().Add(time.Hour)
	if err = m.Notifications().Deliver(ctx, now); err != nil {
		t.Fatal(err)
	}
	f.mu.Lock()
	bc, fc := f.barkCalls, f.feishuCalls
	f.barkFail = true
	f.mu.Unlock()
	if bc != 0 || fc != 1 {
		t.Fatal("disabled channel retried or Feishu failed", bc, fc)
	}
	expectStatus(t, send(t, router, "POST", fmt.Sprintf("/api/v1/channels/%d/enable", bark.ID), cfg.APIToken, nil), 200)
	for i := 0; i < 12; i++ {
		if err = m.Notifications().Deliver(ctx, now.Add(time.Duration(i)*time.Hour)); err != nil {
			t.Fatal(err)
		}
	}
	merged, _ = store.Event(ctx, events[0].ID)
	failedID := int64(0)
	for _, d := range merged.Deliveries {
		if d.ChannelID == bark.ID {
			if d.Status != "failed" || d.Attempts != 5 || d.Error == "" {
				t.Fatal("retry limit not enforced", d)
			}
			failedID = d.ID
		}
	}
	if failedID == 0 {
		t.Fatal("missing failed delivery")
	}
	expectStatus(t, send(t, router, "POST", fmt.Sprintf("/api/v1/deliveries/%d/retry", failedID), cfg.APIToken, nil), 200)
	f.mu.Lock()
	f.barkFail = false
	f.mu.Unlock()
	if err = m.Notifications().Deliver(ctx, now.Add(24*time.Hour)); err != nil {
		t.Fatal(err)
	}
	delivered, _ := store.Delivery(ctx, failedID)
	if delivered.Status != "sent" {
		t.Fatal(delivered)
	}
	// 白名单外新增内容仍保存；关闭渠道后匹配内容不为该渠道新增投递。
	blocked := repository.Post{TID: 1001, PID: 6000, Key: "pid:6000", Kind: "reply", AuthorUID: 3000, Body: "unmatched"}
	if err = store.Transaction(ctx, func(ctx context.Context, tx *repository.Store) error {
		if _, e := tx.InsertPosts(ctx, []repository.Post{blocked}); e != nil {
			return e
		}
		return m.Notifications().Record(ctx, tx, watch, []repository.Post{blocked}, false)
	}); err != nil {
		t.Fatal(err)
	}
	unchanged, _ := store.Inbox(ctx, 1)
	if len(unchanged) != 2 {
		t.Fatal("author whitelist admitted unmatched content")
	}
	expectStatus(t, send(t, router, "POST", fmt.Sprintf("/api/v1/channels/%d/disable", bark.ID), cfg.APIToken, nil), 200)
	blocked.PID, blocked.Key, blocked.AuthorUID = 6001, "pid:6001", 2005
	if err = store.Transaction(ctx, func(ctx context.Context, tx *repository.Store) error {
		if _, e := tx.InsertPosts(ctx, []repository.Post{blocked}); e != nil {
			return e
		}
		return m.Notifications().Record(ctx, tx, watch, []repository.Post{blocked}, false)
	}); err != nil {
		t.Fatal(err)
	}
	latest, _ := store.Inbox(ctx, 1)
	if len(latest) != 3 || len(latest[0].Deliveries) != 1 || latest[0].Deliveries[0].ChannelID != feishu.ID {
		t.Fatal("disabled channel received a new delivery")
	}
	expectStatus(t, send(t, router, "POST", fmt.Sprintf("/api/v1/inbox/%d/read", events[0].ID), cfg.APIToken, nil), 200)
	expectStatus(t, send(t, router, "POST", "/api/v1/feishu-app", cfg.APIToken, infrastructure.AppCredentials{AppID: "cli_fixture_updated"}), 200)
	appCredentials, err := m.Notifications().AppCredentials(ctx)
	if err != nil || appCredentials.AppID != "cli_fixture_updated" || appCredentials.AppSecret != "fixture-app-secret" {
		t.Fatal("application update did not preserve the existing secret", err)
	}
	page := request(t, router, "GET", "/admin/notifications", "")
	expectStatus(t, page, 200)
	response = request(t, router, "GET", "/api/v1/notifications", cfg.APIToken)
	expectStatus(t, response, 200)
	if app := payload[service.NotificationOverview](t, response).App; !app.Configured || app.AppID != "cli_fixture_updated" {
		t.Fatal("configured application ID is missing from the overview")
	}
	m.Close()
	for _, secret := range []string{"fixture-device-secret", "fixture-app-secret", "fixture-recipient-secret", "fixture-tenant-secret"} {
		if bytes.Contains(logs.Bytes(), []byte(secret)) || strings.Contains(response.Body.String(), secret) || strings.Contains(page.Body.String(), secret) {
			t.Fatal("notification secret leaked", secret)
		}
	}
	if err = store.Close(ctx); err != nil {
		t.Fatal(err)
	}
	reopenedRouter, reopened, _ := openAppWithTransport(t, cfg, io.Discard, f)
	response = request(t, reopenedRouter, "GET", "/api/v1/notifications", cfg.APIToken)
	expectStatus(t, response, 200)
	if app := payload[service.NotificationOverview](t, response).App; !app.Configured || app.AppID != "cli_fixture_updated" {
		t.Fatal("application ID lost on restart")
	}
	restored, err := reopened.Event(ctx, events[0].ID)
	if err != nil || !restored.Read {
		t.Fatal("read state lost on restart", err)
	}
	delivered, _ = reopened.Delivery(ctx, failedID)
	if delivered.Status != "sent" {
		t.Fatal("delivery result lost on restart")
	}
}
func TestFeishuImageFailureKeepsText(t *testing.T) {
	f := &noticeFixture{}
	n := infrastructure.NewNotifier(f)
	defer n.Close()
	err := n.Send(context.Background(), "feishu", infrastructure.ChannelTarget{ReceiveID: "fixture", ReceiveIDType: "chat_id"}, infrastructure.AppCredentials{AppID: "cli_image_fixture", AppSecret: "fixture"}, infrastructure.Notice{Title: "Fixture", Text: "Readable text", URL: "https://bbs.nga.cn/read.php?tid=1&pid=2", Images: []string{"https://img.nga.cn/image.png", "https://img.nga.cn/invalid.png"}}, "image-test")
	if err != nil {
		t.Fatal(err)
	}
	if f.feishuCalls != 1 || !strings.Contains(f.message, "Readable text") || !strings.Contains(f.message, "pid=2") {
		t.Fatal("image failure prevented text or reply link delivery", f.message)
	}
}
