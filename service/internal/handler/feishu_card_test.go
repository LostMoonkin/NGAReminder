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
		ImgKey  string `json:"img_key"`
		Alt     feishuCardText
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
	for _, tc := range []struct{ name, kind, author, wantTitle string }{
		{"tid", "tid", "Nickname", "[b]Stored thread title[/b]"},
		{"uid", "uid", "Nickname", "用户监控：Nickname"},
		{"uid_unknown_author", "uid", "", "用户监控：UID 2001"},
		{"uid_long_author", "uid", strings.Repeat("中", 90), "用户监控：" + strings.Repeat("中", 75) + "…"},
	} {
		t.Run(tc.name, func(t *testing.T) {
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
			post := repository.Post{TID: 1001, PID: 4001, Key: "pid:4001", Kind: "reply", Floor: 7, AuthorUID: 2001, Author: "Nickname", Body: "[b]Reply to [pid=4000,1001,1]Reply[/pid] Post by [uid=2002]Quoted user[/uid] (2026-09-20 19:32)[/b]<br/>First line<br/>Second line", SourceURL: "https://bbs.nga.cn/read.php?tid=1001&pid=4001"}
			post.Author = tc.author
			post.Body += strings.Repeat("中🙂", 800)
			post.Resources = []string{"https://img.nga.cn/not-an-inline-image.mp3"}
			watch := repository.Watch{ID: 1, Kind: tc.kind, TID: post.TID, UID: post.AuthorUID, ChannelIDs: []int64{channel.ID}}
			if err = store.Transaction(ctx, func(ctx context.Context, tx *repository.Store) error {
				if err := tx.UpsertThread(ctx, repository.Thread{TID: post.TID, Title: "[b]Stored thread title[/b]", Coverage: "full"}); err != nil {
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
			if card.Header.Title.Tag != "plain_text" || card.Header.Title.Content != tc.wantTitle {
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
			wantBody := fmt.Sprintf("%s · #%d\n\n%s", post.Author, post.Floor, "**Reply to Reply Post by Quoted user (2026-09-20 19:32)**\nFirst line\nSecond line"+strings.Repeat("中🙂", 800)+"\n")
			wantBody = strings.TrimSpace(wantBody) + "\n"
			if body.Content != wantBody {
				t.Errorf("body spacing changed: got %q, want %q", body.Content, wantBody)
			}
			if f.imageDownloads != 0 {
				t.Error("non-inline resources were requested as card images")
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

func TestFeishuCardImagesAndLimitsMatchRust(t *testing.T) {
	f := &noticeFixture{imageUploadOK: true}
	n := infrastructure.NewNotifier(f)
	defer n.Close()
	app := infrastructure.AppCredentials{AppID: "cli_card_images", AppSecret: "fixture"}
	notice := infrastructure.Notice{
		Title: strings.Repeat("中🙂", 45),
		Text: strings.Repeat("中🙂", 1100) + "[img]https://img.nga.cn/image.png[/img]" +
			"[img]https://img.nga.cn/invalid.png[/img][img]https://img.nga.cn/image.png[/img]" +
			"[img]https://img.nga.cn/fourth.png[/img]",
		URL: "https://bbs.nga.cn/read.php?tid=1001&pid=4001",
	}
	for index := 0; index < 3; index++ {
		if index == 2 {
			app.AppID = "cli_different_card_app"
		}
		if err := n.Send(context.Background(), "feishu", infrastructure.ChannelTarget{ReceiveID: "fixture", ReceiveIDType: "chat_id"}, app, notice, ""); err != nil {
			t.Fatal(err)
		}
		card := decodeFeishuCard(t, f.message)
		if card.Header.Title.Content != strings.Repeat("中🙂", 40)+"…" || len(card.Elements) != 5 {
			t.Fatalf("card title or element count diverged from Rust: %+v", card)
		}
		body := card.Elements[0].Text.Content
		if len([]rune(body)) != 2000 || !strings.HasSuffix(body, "…\n\n内容较长，点击“查看帖子”查看完整内容。") {
			t.Fatalf("long card text was not truncated at 2000 characters: %q", body)
		}
		for _, element := range card.Elements[1:3] {
			if element.Tag != "img" || element.ImgKey != "fixture-image-key" || element.Alt.Tag != "plain_text" || element.Alt.Content != "NGA 帖子图片" {
				t.Fatalf("inline image rendering diverged from Rust: %+v", element)
			}
		}
		fallback := card.Elements[3]
		if fallback.Tag != "div" || fallback.Text.Tag != "lark_md" || fallback.Text.Content != "[图片 1](https://img.nga.cn/invalid.png) · [图片 2](https://img.nga.cn/fourth.png)" {
			t.Fatalf("failed and excess images missing from fallback links: %+v", fallback)
		}
		if card.Elements[4].Tag != "action" {
			t.Fatal("original-post button is not the last element")
		}
		wantDownloads, wantUploads := index+2, 1
		if index == 2 {
			wantDownloads, wantUploads = 5, 2
		}
		if f.imageDownloads != wantDownloads || f.imageUploads != wantUploads {
			t.Fatalf("image limit or app-scoped cache mismatch: downloads=%d uploads=%d", f.imageDownloads, f.imageUploads)
		}
	}
}
