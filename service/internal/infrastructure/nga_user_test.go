package infrastructure

import (
	"context"
	"errors"
	"io"
	"net/http"
	"reflect"
	"strings"
	"testing"
)

func TestUserListHeadersMatchRust(t *testing.T) {
	credentials := Credentials{UID: "2009", Cookie: "C3VK=browser; ngaPassportCid=fixture; ngaPassportUid=2009; ngaPassportUrlencodedUname=fixture-user; lastvisit=stale"}
	for _, replies := range []bool{false, true} {
		for _, page := range []int{1, 2} {
			n := NewNGA("fixture-agent", roundTrip(func(r *http.Request) (*http.Response, error) {
				want := http.Header{
					"User-Agent":     {"fixture-agent"},
					"Sec-Fetch-User": {"?1"},
					"Cookie":         {"ngaPassportUid=2009; ngaPassportUrlencodedUname=fixture-user; ngaPassportCid=fixture"},
				}
				if !reflect.DeepEqual(r.Header, want) {
					t.Errorf("user list headers differ from Rust (replies=%t, page=%d)", replies, page)
				}
				body := fixture(t, "user_topics_page_1")
				if replies {
					body = fixture(t, "user_replies_success")
				}
				return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(string(body))), Header: make(http.Header)}, nil
			}))
			_, err := n.UserPage(context.Background(), credentials, 2001, replies, page)
			n.Close()
			if err != nil {
				t.Fatal(err)
			}
		}
	}
}

func TestUserProtocolFixtures(t *testing.T) {
	first, err := parseUserPage(fixture(t, "user_topics_page_1"), 2001, false, 1)
	if err != nil || !first.HasMore || len(first.Candidates) != 1 || first.Candidates[0].TID != 1001 {
		t.Fatalf("invalid first topic page: %+v %v", first, err)
	}
	last, err := parseUserPage(fixture(t, "user_topics_page_2"), 2001, false, 2)
	if err != nil || last.HasMore || len(last.Candidates) != 1 {
		t.Fatalf("invalid final topic page: %+v %v", last, err)
	}
	replies, err := parseUserPage([]byte(`{"result":{"__ROWS":null,"__R__ROWS_PAGE":2,"__T":[
 {"tid":1001,"authorid":2001,"postdate":1767225660,"__P":{"pid":4000}},
 {"__P":{"tid":1001,"pid":4001,"authorid":2001,"postdate":""}},
 {"__P":{"tid":"1001","pid":"4002","authorid":"2001","postdate":"1767225660"}},
 {"__P":{"tid":1001,"pid":4003,"authorid":2002,"postdate":1767225660}}
 ]}}`), 2001, true, 1)
	if err != nil || len(replies.Candidates) != 1 || replies.Candidates[0].PID != 4002 || !replies.HasMore {
		t.Fatalf("reply placeholders or unrelated authors became candidates: %+v %v", replies, err)
	}
	raw := strings.ReplaceAll(string(fixture(t, "post_by_pid_success")), `"lou": 1`, `"lou": 0`)
	post, err := parsePostByPID([]byte(raw), 1001, 4001)
	if err != nil || post.Kind != "reply" || post.Key != "pid:4001" || !strings.Contains(post.SourceURL, "pid=4001") {
		t.Fatalf("PID detail was reclassified as main: %+v %v", post, err)
	}
	if _, err = parsePostByPID([]byte(raw), 1001, 4002); err == nil {
		t.Fatal("mismatched detail PID accepted")
	}
	for _, body := range []string{`{"result":{}}`, `{"result":{"__T":[]}}`, `{"result":{"__T":[],"__ROWS":0,"__R__ROWS_PAGE":0}}`} {
		if _, err = parseUserPage([]byte(body), 2001, true, 1); err == nil {
			t.Fatal("malformed response established an empty list")
		}
	}
}

func TestReplySearchEmptyTailAndAuth(t *testing.T) {
	n := NewNGA("fixture-agent", roundTrip(func(r *http.Request) (*http.Response, error) {
		status, body := 503, ""
		if r.URL.Query().Get("authorid") == "2002" {
			status, body = 200, `{"code":2048,"msg":"必须登录"}`
		}
		return &http.Response{StatusCode: status, Body: io.NopCloser(strings.NewReader(body)), Header: make(http.Header)}, nil
	}))
	n.interval = 0
	defer n.Close()
	credentials := Credentials{UID: "2001", Cookie: "ngaPassportUid=2001; ngaPassportCid=fixture"}
	_, err := n.UserPage(context.Background(), credentials, 2001, true, 2)
	if !errors.Is(err, ErrNGASearchUnavailable) {
		t.Fatalf("empty tail lost its distinct error classification: %v", err)
	}
	if _, err = n.UserPage(context.Background(), credentials, 2002, true, 2); !errors.Is(err, ErrNGAAuth) {
		t.Fatalf("authentication failure became an empty tail: %v", err)
	}
}
