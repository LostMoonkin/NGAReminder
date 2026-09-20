package infrastructure

import (
	"context"
	"io"
	"net/http"
	"os"
	"reflect"
	"strings"
	"testing"
)

func TestUserProfileHeadersAndGBKMatchRust(t *testing.T) {
	body, err := os.ReadFile("testdata/nga/user_profile_gbk.html")
	if err != nil {
		t.Fatal(err)
	}
	credentials := Credentials{UID: "2009", Cookie: "ngaPassportUid=2009; ngaPassportCid=fixture; other=browser"}
	n := NewNGA("fixture-agent", roundTrip(func(r *http.Request) (*http.Response, error) {
		const profileURL = "https://bbs.nga.cn/nuke.php?func=ucp&uid=2001"
		want := http.Header{
			"Content-Type":    {"application/x-www-form-urlencoded"},
			"User-Agent":      {"fixture-agent"},
			"Accept":          {"application/json, text/javascript, */*; q=0.01"},
			"Accept-Language": {"en-US,en;q=0.9,zh-CN;q=0.8,zh;q=0.7"},
			"Cookie":          {credentials.Cookie},
			"Origin":          {"https://bbs.nga.cn"},
			"Referer":         {profileURL},
		}
		if r.Method != http.MethodGet || r.URL.String() != profileURL || !reflect.DeepEqual(r.Header, want) {
			t.Error("user profile request differs from Rust")
		}
		return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(string(body))), Header: http.Header{"Content-Type": {"text/html; charset=GBK"}}}, nil
	}))
	defer n.Close()
	profile, err := n.UserProfile(context.Background(), credentials, 2001)
	if err != nil || profile.UID != 2001 || profile.Username != "脱敏用户" {
		t.Fatalf("GBK profile username was lost: %+v %v", profile, err)
	}
}

func TestUserProfileRejectsMissingOrMismatchedUser(t *testing.T) {
	missing, err := os.ReadFile("testdata/nga/invalid_uid_profile_gbk.html")
	if err != nil {
		t.Fatal(err)
	}
	for _, body := range []string{
		string(missing),
		`var __UCPUSER = {"uid":2002,"username":"wrong user"};`,
		`var __UCPUSER = {"username":"missing uid"};`,
		`var __UCPUSER = {"uid":2001,`,
	} {
		if _, err := parseUserProfile([]byte(body), 2001); err == nil {
			t.Error("invalid user profile accepted")
		}
	}
	profile, err := parseUserProfile([]byte(`var __UCPUSER = {"uid":"2001","username":"fixture } \"user\""}; ignored();`), 2001)
	if err != nil || profile.Username != `fixture } "user"` {
		t.Fatalf("escaped username or trailing script broke profile parsing: %+v %v", profile, err)
	}
}
