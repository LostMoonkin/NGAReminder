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
		t.Fatalf("主楼、普通回复或热门引用去重不正确：%+v", page)
	}
	for i, rawTarget := range []string{"4001", "2002"} {
		p := page.Posts[i+2]
		if p.Kind != "comment" || p.ParentKey != "pid:4001" || p.ParentFloor != 1 || p.CommentToID != rawTarget || p.Floor != 0 {
			t.Fatalf("楼中楼的 JSON 父关系被 comment_to_id 混淆：%+v", p)
		}
		if p.PublishedAt == nil || p.PublishedAt.Location().String() != "UTC" || !strings.Contains(p.SourceURL, "pid=500") {
			t.Fatalf("帖子元数据丢失：%+v", p)
		}
	}
	page, err = parseThreadPage(fixture(t, "thread_attachments"), 1004, 1)
	if err != nil {
		t.Fatal(err)
	}
	if len(page.Posts[0].Resources) != 4 || page.Posts[0].Resources[0] != "https://example.invalid/attachments/synthetic/asset-1.jpg" {
		t.Fatalf("资源地址未完整保留：%v", page.Posts[0].Resources)
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
			t.Fatal("缺失或不一致的主题页被误判成功")
		}
	}
	// 数字字符串是 NGA 已有协议形态，不应丢掉实际楼层。
	raw := strings.ReplaceAll(string(fixture(t, "thread_page_success")), `"lou": 1`, `"lou": "27"`)
	page, err = parseThreadPage([]byte(raw), 1001, 1)
	if err != nil || page.Posts[1].Floor != 27 {
		t.Fatalf("未保留实际楼层：%+v %v", page, err)
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
			body = "公开资料可以读取"
		}
		if r.URL.Path == "/thread.php" {
			if r.Method != "GET" || r.URL.Query().Get("searchpost") != "1" || r.URL.Query().Get("page") != "1" || r.UserAgent() != "fixture-agent" {
				t.Error("未按契约发送回复校验请求")
			}
			if strings.Contains(r.Header.Get("Cookie"), "unrelated") {
				t.Error("用户搜索携带了不必要的浏览器 Cookie")
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
			t.Fatalf("无效 CID 被公开资料误判为可用：%v", err)
		}
	}
	if profileCalls != 0 {
		t.Fatal("不应调用公开资料接口判断认证")
	}
	if _, err := ParseCredentials("", "2001", "a=b=="); err != nil {
		t.Fatalf("Cookie 值中的等号合法：%v", err)
	}
	if _, err := ParseCredentials("", "2001", "secret\nInjected: x"); err == nil {
		t.Fatal("接受了包含换行的 Cookie header")
	}
}
