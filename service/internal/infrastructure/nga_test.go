package infrastructure

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"os"
	"strings"
	"testing"
)

func fixture(t *testing.T, name string) []byte {
	t.Helper()
	body, err := os.ReadFile("testdata/nga/" + name + ".json")
	if err != nil {
		t.Fatal(err)
	}
	return body
}

func TestThreadFixtures(t *testing.T) {
	page, err := parseThreadPage(fixture(t, "thread_comments_hot_post"), 1001, 1)
	if err != nil {
		t.Fatal(err)
	}
	if len(page.Posts) != 4 || page.Posts[0].Key != "main" || page.Posts[1].Floor != 1 {
		t.Fatalf("incorrect main post, reply, or hot-post deduplication: %+v", page)
	}
	for i, rawTarget := range []string{"4001", "2002"} {
		p := page.Posts[i+2]
		if p.Kind != "comment" || p.ParentKey != "pid:4001" || p.ParentFloor != 1 || p.CommentToID != rawTarget || p.Floor != 0 {
			t.Fatalf("comment_to_id incorrectly replaced the nested JSON parent relation: %+v", p)
		}
		if p.PublishedAt == nil || p.PublishedAt.Location().String() != "UTC" || !strings.Contains(p.SourceURL, "pid=500") {
			t.Fatalf("post metadata was lost: %+v", p)
		}
	}
	page, err = parseThreadPage(fixture(t, "thread_attachments"), 1004, 1)
	if err != nil {
		t.Fatal(err)
	}
	if len(page.Posts[0].Resources) != 4 || page.Posts[0].Resources[0] != "https://example.invalid/attachments/synthetic/asset-1.jpg" {
		t.Fatalf("resource URLs were not fully preserved: %v", page.Posts[0].Resources)
	}
	for _, change := range []func(map[string]any){
		func(v map[string]any) { v["result"] = []any{} },
		func(v map[string]any) { v["currentPage"] = 2 },
		func(v map[string]any) { v["result"].([]any)[0].(map[string]any)["tid"] = 999 },
		func(v map[string]any) { delete(v["result"].([]any)[0].(map[string]any), "lou") },
	} {
		var v map[string]any
		if err = json.Unmarshal(fixture(t, "thread_page_success"), &v); err != nil {
			t.Fatal(err)
		}
		change(v)
		raw, _ := json.Marshal(v)
		if _, err = parseThreadPage(raw, 1001, 1); err == nil {
			t.Fatal("missing or inconsistent thread page was treated as successful")
		}
	}
	// 数字字符串是 NGA 已有协议形态，不应丢掉实际楼层。
	raw := strings.ReplaceAll(string(fixture(t, "thread_page_success")), `"lou": 1`, `"lou": "27"`)
	page, err = parseThreadPage([]byte(raw), 1001, 1)
	if err != nil || page.Posts[1].Floor != 27 {
		t.Fatalf("actual floor was not preserved: %+v %v", page, err)
	}
}

type roundTrip func(*http.Request) (*http.Response, error)

func (f roundTrip) RoundTrip(r *http.Request) (*http.Response, error) { return f(r) }

func TestCredentialCheckUsesAuthenticatedReplies(t *testing.T) {
	profileCalls := 0
	n := NewNGA("fixture-agent", roundTrip(func(r *http.Request) (*http.Response, error) {
		body := `{"code":2048,"msg":"必须登录"}`
		if r.URL.Path == "/nuke.php" {
			profileCalls++
			body = "Public profile is accessible"
		}
		if r.URL.Path == "/thread.php" {
			if r.Method != "GET" || r.URL.Query().Get("searchpost") != "1" || r.URL.Query().Get("page") != "1" || r.UserAgent() != "fixture-agent" {
				t.Error("replies validation request did not match the protocol contract")
			}
			if strings.Contains(r.Header.Get("Cookie"), "unrelated") {
				t.Error("user search included unnecessary browser cookies")
			}
			if strings.Contains(r.Header.Get("Cookie"), "ngaPassportCid=valid") {
				body = string(fixture(t, "user_replies_success"))
			}
		}
		return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(body)), Header: make(http.Header)}, nil
	}))
	n.interval = 0
	defer n.Close()
	for _, cid := range []string{"valid", "invalid"} {
		credentials, err := ParseCredentials("Cookie: ngaPassportUid=2001; ngaPassportCid="+cid+"; unrelated=browser;", "", "")
		if err != nil {
			t.Fatal(err)
		}
		err = n.CheckCredentials(context.Background(), credentials)
		if cid == "valid" && err != nil {
			t.Fatal(err)
		}
		if cid == "invalid" && !errors.Is(err, ErrNGAAuth) {
			t.Fatalf("public profile access caused an invalid CID to be accepted: %v", err)
		}
	}
	if profileCalls != 0 {
		t.Fatal("public profile endpoint must not be used to validate credentials")
	}
	if _, err := ParseCredentials("", "2001", "a=b=="); err != nil {
		t.Fatalf("equals signs are valid in Cookie values: %v", err)
	}
	if _, err := ParseCredentials("", "2001", "secret\nInjected: x"); err == nil {
		t.Fatal("Cookie header containing a newline was accepted")
	}
}
