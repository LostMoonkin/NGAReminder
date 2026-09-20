package handler

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"strings"
	"testing"
	"time"

	"ngareminder/service/internal/infrastructure"
	"ngareminder/service/internal/repository"
	"ngareminder/service/internal/service"
)

type feishuCardText struct {
	Tag, Content string
}

type feishuCard struct {
	Header struct {
		Template string
		Title    feishuCardText
	}
	Elements []struct {
		Tag     string
		Text    feishuCardText
		Actions []struct {
			Tag, Type, URL string
			Text           feishuCardText
		}
	}
}

func decodeFeishuCard(t *testing.T, message string) feishuCard {
	t.Helper()
	var envelope struct {
		MsgType string `json:"msg_type"`
		Content string
	}
	if err := json.Unmarshal([]byte(message), &envelope); err != nil {
		t.Fatal(err)
	}
	if envelope.MsgType != "interactive" {
		t.Fatalf("expected interactive card, got %q", envelope.MsgType)
	}
	var card feishuCard
	if err := json.Unmarshal([]byte(envelope.Content), &card); err != nil {
		t.Fatal(err)
	}
	return card
}

func TestFeishuDeliveredCardPreservesOriginalStyle(t *testing.T) {
	for _, kind := range []string{"tid", "uid"} {
		t.Run(kind, func(t *testing.T) {
			cfg := testConfig(t)
			cfg.BackgroundEnabled = true
			f := &noticeFixture{}
			_, store, m := openAppWithTransport(t, cfg, io.Discard, f)
			ctx := context.Background()
			if _, err := m.Notifications().SaveApp(ctx, infrastructure.AppCredentials{AppID: "cli_card_fixture", AppSecret: "fixture"}); err != nil {
				t.Fatal(err)
			}
			channel, err := m.Notifications().SaveChannel(ctx, 0, service.ChannelInput{Name: "Feishu", Kind: "feishu", Enabled: true, ChannelTarget: infrastructure.ChannelTarget{ReceiveID: "fixture", ReceiveIDType: "chat_id"}})
			if err != nil {
				t.Fatal(err)
			}
			post := repository.Post{TID: 1001, PID: 4001, Key: "pid:4001", Kind: "reply", Floor: 7, AuthorUID: 2001, Author: "Nickname", Body: "First line\nSecond line", SourceURL: "https://bbs.nga.cn/read.php?tid=1001&pid=4001"}
			watch := repository.Watch{ID: 1, Kind: kind, TID: post.TID, UID: post.AuthorUID, ChannelIDs: []int64{channel.ID}}
			if err = store.Transaction(ctx, func(ctx context.Context, tx *repository.Store) error {
				if err := tx.UpsertThread(ctx, repository.Thread{TID: post.TID, Title: "Stored thread title", Coverage: "full"}); err != nil {
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
			card := decodeFeishuCard(t, message)
			if card.Header.Template != "blue" {
				t.Errorf("blue title bar missing: template=%q", card.Header.Template)
			}
			wantTitle := "Stored thread title"
			if kind == "uid" {
				wantTitle = "用户监控：Nickname"
			}
			if card.Header.Title.Content != wantTitle {
				t.Errorf("wrong notification title: %q", card.Header.Title.Content)
			}
			if len(card.Elements) != 2 {
				t.Fatalf("expected body and action, got %d elements", len(card.Elements))
			}
			body := card.Elements[0].Text
			if body.Tag != "lark_md" {
				t.Errorf("original markdown rendering missing: %q", body.Tag)
			}
			if strings.Contains(body.Content, post.SourceURL) {
				t.Error("original-post URL was appended to the card body")
			}
			wantBody := fmt.Sprintf("%s · #%d\n\n%s", post.Author, post.Floor, post.Body)
			if body.Content != wantBody {
				t.Errorf("body spacing changed: got %q, want %q", body.Content, wantBody)
			}
			action := card.Elements[1]
			if action.Tag != "action" || len(action.Actions) != 1 {
				t.Fatal("missing original-post action")
			}
			button := action.Actions[0]
			if button.Type != "primary" || button.Text.Content != "查看帖子" || button.URL != post.SourceURL {
				t.Errorf("original primary button changed: %+v", button)
			}
		})
	}
}
