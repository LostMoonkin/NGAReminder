package handler

import (
	"context"
	"fmt"
	"io"
	"net/url"
	"strings"
	"testing"

	"ngareminder/service/internal/repository"
)

func TestInboxFiltersBeforePaginationAndPreservesReturnContext(t *testing.T) {
	router, store := openApp(t, testConfig(t), io.Discard)
	ctx := context.Background()
	watch := repository.Watch{ID: 1, Kind: "tid", TID: 1001, Label: "fixture watch"}
	for i := int64(1); i <= 60; i++ {
		post := repository.Post{ID: i, TID: 1001, Key: fmt.Sprintf("pid:%d", i), PID: i, Kind: "reply", Subject: fmt.Sprintf("message-%02d", i), Body: "fixture content"}
		if _, err := store.InsertPosts(ctx, []repository.Post{post}); err != nil {
			t.Fatal(err)
		}
		event, err := store.MatchEvent(ctx, i, watch)
		if err != nil {
			t.Fatal(err)
		}
		if i > 5 {
			if err = store.MarkRead(ctx, event.ID, true); err != nil {
				t.Fatal(err)
			}
		}
	}
	// 两个待处理投递属于同一旧事件；不能重复消息，也不能被前 50 条已读消息挡住。
	for _, delivery := range []repository.Delivery{
		{EventID: 1, ChannelID: 1, Status: "pending"},
		{EventID: 1, ChannelID: 2, Status: "failed"},
		{EventID: 2, ChannelID: 1, Status: "sent"},
		{EventID: 3, ChannelID: 1, Status: "cancelled"},
	} {
		if err := store.SaveDelivery(ctx, &delivery); err != nil {
			t.Fatal(err)
		}
	}
	unread, err := store.FilteredInbox(ctx, 1, "unread")
	if err != nil || len(unread) != 5 || unread[0].ID != 5 {
		t.Fatalf("unread filter was applied after pagination: %+v, %v", unread, err)
	}
	deliveries, err := store.FilteredInbox(ctx, 1, "delivery")
	if err != nil || len(deliveries) != 1 || deliveries[0].ID != 1 {
		t.Fatalf("delivery filter duplicated or omitted the event: %+v, %v", deliveries, err)
	}
	response := request(t, router, "GET", "/admin/inbox?filter=unread&event=1", "")
	expectStatus(t, response, 200)
	if !strings.Contains(response.Body.String(), "message-01") || strings.Contains(response.Body.String(), "message-60") {
		t.Fatal("web inbox did not render the filtered messages")
	}
	expectStatus(t, request(t, router, "GET", "/admin/inbox?filter=unknown", ""), 400)
	expectStatus(t, request(t, router, "GET", "/admin/inbox?filter=delivery", ""), 200)
	response = send(t, router, "POST", "/admin/inbox/1/read", "", url.Values{
		"return_page": {"2"}, "return_filter": {"unread"}, "return_event": {"1"},
	})
	expectStatus(t, response, 303)
	if response.Header().Get("Location") != "/admin/inbox?page=2&filter=unread&event=1" {
		t.Fatal("mark-read action lost the inbox context")
	}
	response = send(t, router, "POST", "/admin/inbox/1/unread", "", url.Values{
		"return_page": {"-1"}, "return_filter": {"https://example.invalid"}, "return_event": {"-9"},
	})
	expectStatus(t, response, 303)
	if response.Header().Get("Location") != "/admin/inbox?page=1&filter=all&event=0" {
		t.Fatal("inbox return context was not restricted to a local route")
	}
}
