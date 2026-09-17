package handler

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"net/http"
	"strings"
	"testing"
	"time"

	larkim "github.com/larksuite/oapi-sdk-go/v3/service/im/v1"
	"ngareminder/service/internal/infrastructure"
	"ngareminder/service/internal/service"
)

type botFixture struct {
	*noticeFixture
	replies []string
}

func (f *botFixture) RoundTrip(r *http.Request) (*http.Response, error) {
	if strings.HasSuffix(r.URL.Path, "/reply") {
		body, _ := io.ReadAll(r.Body)
		f.mu.Lock()
		f.replies = append(f.replies, string(body))
		f.mu.Unlock()
		return noticeResponse(`{"code":0,"data":{"message_id":"reply"}}`), nil
	}
	if strings.Contains(r.URL.Path, "/ws/endpoint") {
		return noticeResponse(`{"code":1,"msg":"fixture connection rejection"}`), nil
	}
	return f.noticeFixture.RoundTrip(r)
}
func receiveFixture(t *testing.T, h *Handler, id, actor, chat, kind, text string) {
	t.Helper()
	raw, _ := json.Marshal(map[string]any{"header": map[string]any{"app_id": "cli_bot_fixture", "event_type": "im.message.receive_v1"}, "event": map[string]any{"sender": map[string]any{"sender_type": "user", "sender_id": map[string]string{"open_id": actor}}, "message": map[string]string{"message_id": id, "chat_id": chat, "chat_type": kind, "message_type": "text", "content": string(mustJSON(map[string]string{"text": text}))}}})
	var event larkim.P2MessageReceiveV1
	if err := json.Unmarshal(raw, &event); err != nil {
		t.Fatal(err)
	}
	if err := h.receiveBot(context.Background(), &event); err != nil {
		t.Fatal(err)
	}
}
func mustJSON(v any) []byte { raw, _ := json.Marshal(v); return raw }
func TestBotBindingCommandsAndReplay(t *testing.T) {
	cfg := testConfig(t)
	cfg.BackgroundEnabled = true
	var logs bytes.Buffer
	f := &botFixture{noticeFixture: &noticeFixture{nga: fixtureNGA(t)}}
	router, store, m := openAppWithTransport(t, cfg, &logs, f)
	h := &Handler{monitor: m}
	ctx := context.Background()
	expectStatus(t, send(t, router, "POST", "/api/v1/feishu-app", cfg.APIToken, infrastructure.AppCredentials{AppID: "cli_bot_fixture", AppSecret: "fixture-bot-secret"}), 200)
	expectStatus(t, send(t, router, "POST", "/api/v1/bot", cfg.APIToken, map[string]any{"enabled": true, "groups": "allowed-group"}), 200)
	response := send(t, router, "POST", "/api/v1/bot/bind-code", cfg.APIToken, nil)
	expectStatus(t, response, 200)
	code := payload[struct {
		Code string `json:"code"`
	}](t, response).Code
	receiveFixture(t, h, "unbound", "stranger", "private", "p2p", "/status")
	receiveFixture(t, h, "bind-group", "owner", "allowed-group", "group", "/bind "+code)
	receiveFixture(t, h, "bind-private", "owner", "private", "p2p", "/bind "+code)
	receiveFixture(t, h, "reuse-code", "other", "private2", "p2p", "/bind "+code)
	bindings, err := store.Bindings(ctx)
	if err != nil || len(bindings) != 1 {
		t.Fatal("binding code was not single-use/private", bindings, err)
	}
	for _, cmd := range []string{"/help", "/status", "/watch list", "/watch", "/unknown"} {
		receiveFixture(t, h, "query-"+cmd, "owner", "private", "p2p", cmd)
	}
	expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, service.AccountInput{PassportUID: "2001", PassportCID: "fixture-valid-secret"}), 200)
	watch := createWatch(t, router, cfg.APIToken, 1001, "full")
	receiveFixture(t, h, "wrong-group", "owner", "denied-group", "group", "/watch run 1")
	receiveFixture(t, h, "accepted", "owner", "allowed-group", "group", "/watch run 1")
	receiveFixture(t, h, "accepted", "owner", "allowed-group", "group", "/watch run 1")
	run := awaitRun(t, store, watch.ID)
	if run.Status != "success" {
		t.Fatal(run)
	}
	runs, _ := store.WatchRuns(ctx, watch.ID)
	if len(runs) != 1 {
		t.Fatal("replayed command created duplicate runs")
	}
	response = send(t, router, "POST", "/api/v1/bot/bind-code", cfg.APIToken, nil)
	expectStatus(t, response, 200)
	expiredCode := payload[struct {
		Code string `json:"code"`
	}](t, response).Code
	settings, _ := store.BotSettings(ctx)
	past := time.Now().Add(-time.Minute)
	settings.CodeExpires = &past
	if err = store.SaveBotSettings(ctx, &settings); err != nil {
		t.Fatal(err)
	}
	receiveFixture(t, h, "expired", "late", "private", "p2p", "/bind "+expiredCode)
	expectStatus(t, request(t, router, "GET", "/admin/bot", ""), 200)
	m.Close()
	if err = store.Close(ctx); err != nil {
		t.Fatal(err)
	}
	_, reopened, restarted := openAppWithTransport(t, cfg, io.Discard, f)
	h = &Handler{monitor: restarted}
	receiveFixture(t, h, "accepted", "owner", "allowed-group", "group", "/watch run 1")
	runs, _ = reopened.WatchRuns(ctx, watch.ID)
	if len(runs) != 1 {
		t.Fatal("restart lost message deduplication")
	}
	if err = restarted.Bot().Revoke(ctx, bindings[0].ID); err != nil {
		t.Fatal(err)
	}
	receiveFixture(t, h, "revoked", "owner", "private", "p2p", "/watch run 1")
	runs, _ = reopened.WatchRuns(ctx, watch.ID)
	if len(runs) != 1 {
		t.Fatal("revoked actor could run a watch")
	}
	for _, secret := range []string{code, expiredCode, "fixture-bot-secret", "allowed-group", "/bind " + code} {
		if bytes.Contains(logs.Bytes(), []byte(secret)) {
			t.Fatal("bot logs leaked a secret or raw conversation")
		}
	}
	f.mu.Lock()
	responses := strings.Join(f.replies, "\n")
	f.mu.Unlock()
	for _, text := range []string{"绑定成功", "已接受", "未知命令", "用法", "未获授权"} {
		if !strings.Contains(responses, text) {
			t.Errorf("missing useful bot response %s", text)
		}
	}
}
func TestBotConnectionFailureAndIndependentSwitches(t *testing.T) {
	cfg := testConfig(t)
	cfg.BackgroundEnabled = true
	f := &botFixture{noticeFixture: &noticeFixture{nga: fixtureNGA(t)}}
	_, store, m := openAppWithTransport(t, cfg, io.Discard, f)
	if _, err := m.Notifications().SaveApp(context.Background(), infrastructure.AppCredentials{AppID: "cli_connection_fixture", AppSecret: "fixture"}); err != nil {
		t.Fatal(err)
	}
	if _, err := m.Bot().SaveSettings(context.Background(), true, nil); err != nil {
		t.Fatal(err)
	}
	m.Bot().Start()
	for deadline := time.Now().Add(6 * time.Second); time.Now().Before(deadline); {
		s, _ := store.BotSettings(context.Background())
		if s.Status == "failed" || s.Status == "reconnecting" {
			break
		}
		time.Sleep(25 * time.Millisecond)
	}
	s, _ := store.BotSettings(context.Background())
	if s.LastError == "" {
		t.Fatal("connection failure was not visible", s)
	}
	if _, err := m.Bot().SaveSettings(context.Background(), false, nil); err != nil {
		t.Fatal(err)
	}
	channel, err := m.Notifications().SaveChannel(context.Background(), 0, service.ChannelInput{Name: "Bark", Kind: "bark", Enabled: true, ChannelTarget: infrastructure.ChannelTarget{ServerURL: "https://bark.example.invalid", DeviceKey: "fixture"}})
	if err != nil {
		t.Fatal(err)
	}
	if err = m.Notifications().TestChannel(context.Background(), channel.ID); err != nil {
		t.Fatal("disabled Bot blocked notifications", err)
	}
}
