package infrastructure

import (
	"bytes"
	"context"
	"errors"
	"image"
	"image/png"
	"io"
	"net/http"
	"os"
	"strings"
	"testing"
)

func TestLoginResponseFixtures(t *testing.T) {
	for _, tc := range []struct{ name, uid, code string }{{"login_success", "12345678", ""}, {"login_success_wrapped", "87654321", ""}, {"login_password_error", "", "invalid_credentials"}, {"login_captcha_error", "", "invalid_captcha"}, {"login_busy", "", "busy"}, {"login_empty_error", "", "candidate_cookie_missing"}, {"login_missing_candidate", "", "candidate_cookie_missing"}} {
		raw, err := os.ReadFile("testdata/nga/" + tc.name + ".json")
		if err != nil {
			t.Fatal(err)
		}
		value, err := parseLoginResponse(raw, nil)
		if tc.code == "" {
			if err != nil || value.UID != tc.uid {
				t.Fatal(tc.name, value.UID, err)
			}
		} else {
			var e *LoginError
			if !errors.As(err, &e) || e.Code != tc.code {
				t.Fatal(tc.name, err)
			}
		}
	}
	value, err := parseLoginResponse([]byte(`{"code":0,"data":[]}`), []*http.Cookie{{Name: "ngaPassportUid", Value: "2001"}, {Name: "ngaPassportCid", Value: "fixture-response-cookie"}})
	if err != nil || value.UID != "2001" {
		t.Fatal("explicit response cookies were not accepted", err)
	}
	_, err = parseLoginResponse([]byte(`{"error":["密码错误"]}`), []*http.Cookie{{Name: "ngaPassportUid", Value: "2001"}, {Name: "ngaPassportCid", Value: "fixture-response-cookie"}})
	if err == nil {
		t.Fatal("error response cookies were accepted")
	}
}

func TestPrepareLoginWithArchivedAccountPage(t *testing.T) {
	account, err := os.ReadFile("../../../archive/rust-service/service/tests/fixtures/nga/account_page.html")
	if err != nil {
		t.Fatal(err)
	}
	var captcha bytes.Buffer
	if err = png.Encode(&captcha, image.NewRGBA(image.Rect(0, 0, 1, 1))); err != nil {
		t.Fatal(err)
	}
	client := NewNGA("fixture-agent", roundTrip(func(r *http.Request) (*http.Response, error) {
		body, mime := "entry", "text/html"
		switch r.URL.Path {
		case "/nuke/account_copy.html":
			body = string(account)
		case "/login_check_code.php":
			body, mime = captcha.String(), "image/png"
		}
		return &http.Response{StatusCode: 200, Header: http.Header{"Content-Type": {mime}}, Body: io.NopCloser(strings.NewReader(body)), Request: r}, nil
	}))
	defer client.Close()
	challenge, _, err := client.PrepareLogin(context.Background())
	if err != nil {
		t.Fatal("archived JavaScript PEM could not prepare login", err)
	}
	challenge.Close()
}
