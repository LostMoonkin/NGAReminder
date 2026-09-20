package infrastructure

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"
	"testing"
	"testing/synctest"
	"time"
)

func TestNGAUserListFollowsRedirect(t *testing.T) {
	for _, replies := range []bool{false, true} {
		calls := 0
		credentials := Credentials{UID: "2001", Cookie: "ngaPassportUid=2001; ngaPassportCid=fixture-secret"}
		n := NewNGA("fixture-agent", roundTrip(func(r *http.Request) (*http.Response, error) {
			calls++
			if calls == 1 {
				return &http.Response{StatusCode: http.StatusFound, Header: http.Header{"Location": {r.URL.RequestURI() + "&redirected=1"}}, Body: http.NoBody}, nil
			}
			if r.URL.Query().Get("redirected") != "1" || r.Header.Get("Cookie") != credentials.Cookie || r.Header.Get("Sec-Fetch-User") != "?1" {
				t.Error("redirect lost the user list query or same-origin headers")
			}
			name := "user_topics_page_1"
			if replies {
				name = "user_replies_success"
			}
			return &http.Response{StatusCode: http.StatusOK, Header: make(http.Header), Body: io.NopCloser(strings.NewReader(string(fixture(t, name))))}, nil
		}))
		n.interval = 0
		page, err := n.UserPage(context.Background(), credentials, 2001, replies, 1)
		n.Close()
		if err != nil || calls != 2 || len(page.Candidates) != 1 {
			t.Fatalf("302 interrupted user collection: replies=%t calls=%d candidates=%d err=%v", replies, calls, len(page.Candidates), err)
		}
	}
}

func TestNGARedirectLimit(t *testing.T) {
	for _, redirects := range []int{10, 11} {
		t.Run(fmt.Sprint(redirects), func(t *testing.T) {
			calls := 0
			n := NewNGA("fixture-agent", roundTrip(func(r *http.Request) (*http.Response, error) {
				calls++
				if calls <= redirects {
					return &http.Response{StatusCode: http.StatusFound, Header: http.Header{"Location": {fmt.Sprintf("/fixture?step=%d", calls)}}, Body: http.NoBody}, nil
				}
				return &http.Response{StatusCode: http.StatusOK, Header: make(http.Header), Body: io.NopCloser(strings.NewReader(`{"code":0}`))}, nil
			}))
			n.interval = 0
			defer n.Close()
			_, err := n.request(context.Background(), http.MethodGet, "/fixture", "", "")
			if calls != 11 || (err != nil) != (redirects > 10) {
				t.Fatalf("redirect limit differs from Rust: redirects=%d calls=%d err=%v", redirects, calls, err)
			}
		})
	}
}

func TestNGARedirectCookieScope(t *testing.T) {
	for _, scenario := range []struct {
		name, target string
		keepCookie   bool
	}{
		{"same_origin", "https://bbs.nga.cn:443/redirected#fragment", true},
		{"subdomain", "https://sub.bbs.nga.cn/redirected", false},
		{"other_host", "https://other.example/redirected", false},
		{"other_port", "https://bbs.nga.cn:8443/redirected", false},
		{"other_scheme", "http://bbs.nga.cn:443/redirected", false},
	} {
		t.Run(scenario.name, func(t *testing.T) {
			calls := 0
			n := NewNGA("fixture-agent", roundTrip(func(r *http.Request) (*http.Response, error) {
				calls++
				if calls == 1 {
					return &http.Response{StatusCode: http.StatusFound, Header: http.Header{"Location": {scenario.target}}, Body: http.NoBody}, nil
				}
				wantCookie := ""
				if scenario.keepCookie {
					wantCookie = "ngaPassportCid=fixture-secret"
				}
				if r.Header.Get("Cookie") != wantCookie {
					t.Errorf("Cookie forwarding differs from Rust at request %d", calls)
				}
				if calls == 2 {
					// 跳回原站也不能恢复已经移除的 Cookie。
					return &http.Response{StatusCode: http.StatusFound, Header: http.Header{"Location": {ngaBaseURL + "/done"}}, Body: http.NoBody}, nil
				}
				wantReferer, _, _ := strings.Cut(scenario.target, "#")
				if r.Header.Get("Referer") != wantReferer {
					t.Error("Referer did not follow the previous URL without its fragment")
				}
				return &http.Response{StatusCode: http.StatusOK, Header: make(http.Header), Body: io.NopCloser(strings.NewReader(`{"code":0}`))}, nil
			}))
			n.interval = 0
			defer n.Close()
			if _, err := n.request(context.Background(), http.MethodGet, "/fixture", "", "ngaPassportCid=fixture-secret"); err != nil || calls != 3 {
				t.Fatalf("redirect chain failed: calls=%d err=%v", calls, err)
			}
		})
	}
}

func TestNGARedirectPostMethod(t *testing.T) {
	for _, status := range []int{301, 302, 303, 307, 308} {
		t.Run(fmt.Sprint(status), func(t *testing.T) {
			calls := 0
			n := NewNGA("fixture-agent", roundTrip(func(r *http.Request) (*http.Response, error) {
				calls++
				if calls == 1 {
					return &http.Response{StatusCode: status, Header: http.Header{"Location": {"/detail"}}, Body: http.NoBody}, nil
				}
				wantMethod, wantBody := http.MethodGet, ""
				if status == 307 || status == 308 {
					wantMethod, wantBody = http.MethodPost, "tid=1001&pid=4001"
				}
				var body []byte
				if r.Body != nil {
					body, _ = io.ReadAll(r.Body)
					_ = r.Body.Close()
				}
				if r.Method != wantMethod || string(body) != wantBody {
					t.Errorf("redirect changed POST incorrectly: method=%s body=%s", r.Method, body)
				}
				return &http.Response{StatusCode: http.StatusOK, Header: make(http.Header), Body: io.NopCloser(strings.NewReader(`{"code":0}`))}, nil
			}))
			n.interval = 0
			defer n.Close()
			if _, err := n.request(context.Background(), http.MethodPost, "/app_api.php", "tid=1001&pid=4001", ""); err != nil || calls != 2 {
				t.Fatalf("redirected detail request failed: calls=%d err=%v", calls, err)
			}
		})
	}
}

func TestNGAUserRedirectPreservesAuthError(t *testing.T) {
	calls := 0
	n := NewNGA("fixture-agent", roundTrip(func(r *http.Request) (*http.Response, error) {
		calls++
		if calls == 1 {
			return &http.Response{StatusCode: http.StatusFound, Header: http.Header{"Location": {"/thread.php?redirected=1"}}, Body: http.NoBody}, nil
		}
		return &http.Response{StatusCode: http.StatusOK, Header: make(http.Header), Body: io.NopCloser(strings.NewReader(`{"code":2048,"msg":"必须登录"}`))}, nil
	}))
	n.interval = 0
	defer n.Close()
	_, err := n.UserPage(context.Background(), Credentials{UID: "2001", Cookie: "ngaPassportCid=fixture-secret"}, 2001, true, 1)
	if !errors.Is(err, ErrNGAAuth) || calls != 2 {
		t.Fatalf("redirect hid the final authentication error: calls=%d err=%v", calls, err)
	}
}

func TestNGARedirectUsesRateLimit(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		var starts []time.Time
		n := NewNGA("fixture-agent", roundTrip(func(r *http.Request) (*http.Response, error) {
			starts = append(starts, time.Now())
			if len(starts) < 3 {
				return &http.Response{StatusCode: http.StatusFound, Header: http.Header{"Location": {"/fixture"}}, Body: http.NoBody}, nil
			}
			return &http.Response{StatusCode: http.StatusOK, Header: make(http.Header), Body: io.NopCloser(strings.NewReader(`{"code":0}`))}, nil
		}))
		defer n.Close()
		if _, err := n.request(context.Background(), http.MethodGet, "/fixture", "", ""); err != nil {
			t.Fatal(err)
		}
		if len(starts) != 3 || starts[1].Sub(starts[0]) != 500*time.Millisecond || starts[2].Sub(starts[1]) != 500*time.Millisecond {
			t.Fatalf("redirects bypassed the global rate limit: %v", starts)
		}
	})
}
