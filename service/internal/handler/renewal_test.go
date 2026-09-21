package handler

import (
	"bytes"
	"context"
	"crypto/rand"
	"crypto/rsa"
	"crypto/x509"
	"encoding/base64"
	"encoding/pem"
	"errors"
	"image"
	"image/png"
	"io"
	"net/http"
	"ngareminder/service/internal/infrastructure"
	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/repository"
	"ngareminder/service/internal/service"
	"strings"
	"sync"
	"testing"
	"time"
)

type loginFixture struct {
	blockPhase string
	entered    chan struct{}
	*botFixture
	key                               *rsa.PrivateKey
	prepared, submitted, checks       int
	imageFail, checkFail, networkFail bool
	response                          string
	notices                           []string
}

var loginFixtureKey = sync.OnceValues(func() (*rsa.PrivateKey, error) {
	return rsa.GenerateKey(rand.Reader, 2048)
})

func newLoginFixture(t *testing.T) *loginFixture {
	t.Helper()
	key, err := loginFixtureKey()
	if err != nil {
		t.Fatal(err)
	}
	return &loginFixture{botFixture: &botFixture{noticeFixture: &noticeFixture{nga: fixtureNGA(t)}}, key: key, response: `{"code":0,"data":[null,null,null,{"uid":"2001","token":"fixture-new-cookie"}]}`}
}
func (f *loginFixture) RoundTrip(r *http.Request) (*http.Response, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if r.URL.Host == "bbs.nga.cn" {
		switch r.URL.Path {
		case "/nuke.php":
			if r.Method == "GET" {
				f.prepared++
				out := fixtureResponse("entry")
				out.Header.Add("Set-Cookie", "login_session=fixture; Path=/; Secure")
				return out, nil
			}
			f.submitted++
			if err := r.ParseMultipartForm(1 << 20); err != nil {
				return nil, err
			}
			encrypted, err := base64.StdEncoding.DecodeString(r.FormValue("password"))
			if err != nil {
				return nil, err
			}
			password, err := rsa.DecryptPKCS1v15(rand.Reader, f.key, encrypted)
			if err != nil {
				return nil, err
			}
			if string(password) != "fixture-login-password" || r.FormValue("name") != "fixture-login-name" || r.FormValue("captcha") != "aB1234" || r.FormValue("__act") != "login" || !strings.Contains(r.Header.Get("Cookie"), "login_session=fixture") {
				return nil, errors.New("login protocol did not preserve the session or encode credentials")
			}
			return fixtureResponse(f.response), nil
		case "/nuke/account_copy.html":
			if f.blockPhase == "prepare" {
				return f.waitCancelled(r)
			}
			if !strings.Contains(r.Header.Get("Cookie"), "login_session=fixture") {
				return nil, errors.New("missing independent login cookie jar")
			}
			key, _ := x509.MarshalPKIXPublicKey(&f.key.PublicKey)
			encoded := string(pem.EncodeToMemory(&pem.Block{Type: "PUBLIC KEY", Bytes: key}))
			return fixtureResponse("var publicKey = \"" + strings.ReplaceAll(encoded, "\n", "\\n\\\r\n") + "\";"), nil
		case "/login_check_code.php":
			var data bytes.Buffer
			_ = png.Encode(&data, image.NewRGBA(image.Rect(0, 0, 1, 1)))
			out := fixtureResponse(data.String())
			out.Header.Set("Content-Type", "image/png")
			return out, nil
		case "/thread.php":
			if f.networkFail {
				return nil, errors.New("fixture network failure")
			}
			if strings.Contains(r.Header.Get("Cookie"), "fixture-new-cookie") {
				f.checks++
				if f.blockPhase == "validate" {
					return f.waitCancelled(r)
				}
				if f.checkFail {
					return fixtureResponse(`{"code":46}`), nil
				}
				return fixtureResponse(`{"code":0,"result":{"__T":[],"__ROWS":null}}`), nil
			}
		}
	} else if strings.HasSuffix(r.URL.Path, "/images") {
		if f.imageFail {
			return noticeResponse(`{"code":123}`), nil
		}
		return noticeResponse(`{"code":0,"data":{"image_key":"fixture-captcha-key"}}`), nil
	} else if strings.HasSuffix(r.URL.Path, "/messages") {
		raw, _ := io.ReadAll(r.Body)
		f.notices = append(f.notices, string(raw))
		return noticeResponse(`{"code":0,"data":{"message_id":"fixture-login-notice"}}`), nil
	}
	// 父 fixture 使用同一把锁。
	f.mu.Unlock()
	out, err := f.botFixture.RoundTrip(r)
	f.mu.Lock()
	return out, err
}
func setupRenewal(t *testing.T, f *loginFixture, writer io.Writer) (http.Handler, *repository.Store, *service.Monitoring, string) {
	t.Helper()
	cfg := testConfig(t)
	cfg.BackgroundEnabled = true
	router, store, m := openAppWithTransport(t, cfg, writer, f)
	ctx := context.Background()
	expectStatus(t, send(t, router, "PUT", "/api/v1/nga-account", cfg.APIToken, service.AccountInput{PassportUID: "2001", PassportCID: "fixture-valid-secret"}), 200)
	if _, err := m.Notifications().SaveApp(ctx, infrastructure.AppCredentials{AppID: "cli_bot_fixture", AppSecret: "fixture-app-secret"}); err != nil {
		t.Fatal(err)
	}
	if _, err := m.Bot().SaveSettings(ctx, true, nil); err != nil {
		t.Fatal(err)
	}
	code, _, err := m.Bot().GenerateCode(ctx)
	if err != nil {
		t.Fatal(err)
	}
	receiveFixture(t, &Handler{monitor: m}, "bind", "owner", "private", "p2p", "/bind "+code)
	bindings, _ := store.Bindings(ctx)
	expectStatus(t, send(t, router, "POST", "/api/v1/renewal", cfg.APIToken, service.RenewalInput{Enabled: true, BindingID: bindings[0].ID, Name: "fixture-login-name", Password: "fixture-login-password"}), 200)
	return router, store, m, cfg.APIToken
}
func loginCommand(m *service.Monitoring, id, actor, chat, kind string, args ...string) (string, error) {
	return m.Bot().Execute(context.Background(), service.BotCommand{AppID: "cli_bot_fixture", MessageID: id, ActorID: actor, ChatID: chat, ChatType: kind, Name: "/login", Args: args})
}
func TestRenewalAuthenticationFlowAndPreservedPause(t *testing.T) {
	f := newLoginFixture(t)
	var logs bytes.Buffer
	router, store, m, token := setupRenewal(t, f, &logs)
	ctx := context.Background()
	watch := createWatch(t, router, token, 1001, "full")
	paused := createWatch(t, router, token, 1002, "full")
	if _, err := m.ChangeWatch(ctx, paused.ID, "pause", ""); err != nil {
		t.Fatal(err)
	}
	f.nga.mu.Lock()
	f.nga.code = 46
	f.nga.mu.Unlock()
	if run := runWatch(t, router, token, watch.ID); run.Status != "auth_paused" {
		t.Fatal(run)
	}
	data, err := m.Renewal().Overview(ctx)
	for deadline := time.Now().Add(3 * time.Second); err == nil && data.Request.ID == "" && time.Now().Before(deadline); {
		time.Sleep(10 * time.Millisecond)
		data, err = m.Renewal().Overview(ctx)
	}
	if err != nil || data.Request.Status != "awaiting_confirmation" {
		t.Fatal(data, err)
	}
	v := data.Request
	repeat, err := m.Renewal().Start(ctx)
	if err != nil || repeat.ID != v.ID {
		t.Fatal("active renewal was not reused", err)
	}
	for _, c := range []struct{ actor, chat, kind, action string }{{"owner", "private", "p2p", "captcha"}, {"stranger", "private", "p2p", "confirm"}, {"owner", "group", "group", "confirm"}} {
		_, _ = loginCommand(m, c.actor+c.kind+c.action, c.actor, c.chat, c.kind, c.action, v.ID, "aB1234")
	}
	if f.prepared != 0 || f.submitted != 0 {
		t.Fatal("unauthorized or unconfirmed request submitted credentials")
	}
	reply, err := loginCommand(m, "confirm", "owner", "private", "p2p", "confirm", v.ID)
	if err != nil || !strings.Contains(reply, "图片已发送") {
		t.Fatal(reply, err)
	}
	reply, err = loginCommand(m, "captcha", "owner", "private", "p2p", "captcha", v.ID, "aB1234")
	if err != nil || !strings.Contains(reply, "续期成功") {
		t.Fatal(reply, err)
	}
	_, err = loginCommand(m, "captcha", "owner", "private", "p2p", "captcha", v.ID, "aB1234")
	if err != nil {
		t.Fatal(err)
	}
	account, _ := store.Account(ctx)
	active, _ := store.Watch(ctx, watch.ID)
	manual, _ := store.Watch(ctx, paused.ID)
	if account.Status != "valid" || active.State != "ready" || !manual.Paused || f.submitted != 1 || f.checks != 1 {
		t.Fatal("renewal did not preserve pause or validate exactly once", account, active, manual)
	}
	data, _ = m.Renewal().Overview(ctx)
	if data.Request.Status != "success" || len(data.Request.Expected) > 0 {
		t.Fatal("renewal context survived completion")
	}
	expectStatus(t, request(t, router, "GET", "/admin/renewal", ""), 200)
	response := send(t, router, "GET", "/api/v1/renewal", token, nil)
	for _, secret := range []string{"fixture-login-password", "fixture-login-name", "aB1234", "fixture-new-cookie", "fixture-valid-secret", "fixture-captcha-key"} {
		if bytes.Contains(logs.Bytes(), []byte(secret)) || bytes.Contains(response.Body.Bytes(), []byte(secret)) {
			t.Fatal("renewal leaked a secret")
		}
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	if len(f.notices) != 2 {
		t.Fatal("duplicate confirmation or missing captcha delivery", len(f.notices))
	}
}
func TestRenewalFailuresKeepOldCookie(t *testing.T) {
	for _, scenario := range []string{"wrong_uid", "invalid_candidate", "password", "unsupported", "image", "cancel", "expire", "restart", "network"} {
		t.Run(scenario, func(t *testing.T) {
			f := newLoginFixture(t)
			router, store, m, token := setupRenewal(t, f, io.Discard)
			ctx := context.Background()
			before, _ := store.Account(ctx)
			if scenario == "network" {
				f.mu.Lock()
				f.networkFail = true
				f.mu.Unlock()
				response := send(t, router, "POST", "/api/v1/nga-account/check", token, nil)
				if response.Code == 200 {
					t.Fatal("network error accepted")
				}
				v, _ := store.LatestRenewal(ctx)
				if v.ID != "" || f.submitted != 0 {
					t.Fatal("network failure triggered renewal")
				}
				return
			}
			response := request(t, router, "POST", "/admin/renewal/start", "")
			expectStatus(t, response, http.StatusSeeOther)
			if response.Header().Get("Location") != "/admin/renewal" {
				t.Fatal("manual renewal did not redirect to its status page")
			}
			v, err := store.LatestRenewal(ctx)
			if err != nil {
				t.Fatal(err)
			}
			switch scenario {
			case "wrong_uid":
				f.response = `{"code":0,"data":[null,null,null,{"uid":"9999","token":"fixture-new-cookie"}]}`
			case "invalid_candidate":
				f.checkFail = true
			case "password":
				f.response = `{"code":1,"error":{"msg":"密码错误"}}`
			case "unsupported":
				f.response = `{"code":1,"error":["请进行手机短信验证"]}`
			case "image":
				f.imageFail = true
			case "cancel":
				_, err = loginCommand(m, "cancel", "owner", "private", "p2p", "cancel", v.ID)
			case "expire":
				v.ExpiresAt = time.Now().Add(-time.Second)
				err = store.SaveRenewal(ctx, &v)
			case "restart":
				m.Close()
				cfg := testConfig(t)
				cfg.BackgroundEnabled = true
				m, err = service.NewMonitoring(ctx, cfg, store, infrastructure.NewNGA(cfg.NGAUserAgent, f), logging.New(io.Discard), infrastructure.NewNotifier(f))
				if err == nil {
					t.Cleanup(m.Close)
				}
			}
			if err != nil {
				t.Fatal(err)
			}
			_, _ = loginCommand(m, "confirm", "owner", "private", "p2p", "confirm", v.ID)
			_, _ = loginCommand(m, "captcha", "owner", "private", "p2p", "captcha", v.ID, "aB1234")
			after, _ := store.Account(ctx)
			data, err := m.Renewal().Overview(ctx)
			if err != nil || !bytes.Equal(before.Cookie, after.Cookie) || before.Status != after.Status || len(data.Request.Expected) > 0 {
				t.Fatal("failure replaced the old Cookie or retained context", scenario, err, data.Request)
			}
			if scenario == "image" || scenario == "cancel" || scenario == "expire" || scenario == "restart" {
				if f.submitted != 0 {
					t.Fatal("ended request submitted credentials")
				}
			}
			if scenario == "wrong_uid" && f.checks != 0 {
				t.Fatal("wrong UID was checked as expected account")
			}
			if scenario == "unsupported" && !strings.Contains(data.Request.Error, "手动更新") {
				t.Fatal("missing unsupported challenge guidance")
			}
			fresh, e := m.Renewal().Start(ctx)
			if e != nil || fresh.ID == v.ID {
				t.Fatal("ended request could not be explicitly restarted", e)
			}
		})
	}
}

func (f *loginFixture) waitCancelled(r *http.Request) (*http.Response, error) {
	close(f.entered)
	f.mu.Unlock()
	<-r.Context().Done()
	f.mu.Lock()
	return nil, r.Context().Err()
}
