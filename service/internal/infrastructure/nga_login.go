package infrastructure

import (
	"bytes"
	"context"
	"crypto/rand"
	"crypto/rsa"
	"crypto/x509"
	"encoding/base64"
	"encoding/json"
	"encoding/pem"
	"errors"
	"fmt"
	"github.com/rs/zerolog"
	"io"
	"math/big"
	"mime/multipart"
	"net/http"
	"net/http/cookiejar"
	"net/url"
	"ngareminder/service/internal/logging"
	"regexp"
	"strconv"
	"strings"
	"time"
)

type LoginError struct{ Code string }

func (e *LoginError) Error() string { return "NGA login: " + e.Code }
func loginError(code string) error  { return logging.WithStack(&LoginError{code}) }

// 仅保存在本次交互的内存中；结束/重启即丢弃，不恢复密码提交。
type LoginChallenge struct {
	client    *http.Client
	userAgent string
	key       *rsa.PublicKey
	rid, prid string
}

func (c *LoginChallenge) Close() { c.client.CloseIdleConnections() }

var publicKeyPattern = regexp.MustCompile(`(?s)-----BEGIN PUBLIC KEY-----.*?-----END PUBLIC KEY-----`)

func (n *NGA) PrepareLogin(ctx context.Context) (challenge *LoginChallenge, image []byte, err error) {
	ctx, span := logging.Start(ctx, "infrastructure.nga.prepare_login")
	defer span.End(&err)
	jar, e := cookiejar.New(nil)
	if e != nil {
		return nil, nil, logging.Wrap(e, "create NGA login cookie jar")
	}
	c := &LoginChallenge{client: &http.Client{Transport: n.http.Transport, Jar: jar, Timeout: 20 * time.Second, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}, userAgent: n.userAgent}
	success := false
	defer func() {
		if !success {
			c.Close()
		}
	}()
	if _, _, err = c.request(ctx, "GET", "/nuke.php?__lib=login&__act=account&login", "", nil, 2_000_000); err != nil {
		return nil, nil, err
	}
	body, _, err := c.request(ctx, "GET", "/nuke/account_copy.html?login", "", nil, 2_000_000)
	if err != nil {
		return nil, nil, err
	}
	raw := publicKeyPattern.Find(body)
	if len(raw) == 0 || len(raw) > 16_384 {
		return nil, nil, loginError("protocol_changed")
	}
	normalized := strings.NewReplacer("\\n\\\r\n", "\n", "\\n\\\n", "\n", "\\n", "\n", "\r", "").Replace(string(raw))
	block, _ := pem.Decode([]byte(normalized))
	if block == nil {
		return nil, nil, loginError("protocol_changed")
	}
	key, e := x509.ParsePKIXPublicKey(block.Bytes)
	if e != nil {
		return nil, nil, loginError("protocol_changed")
	}
	var ok bool
	c.key, ok = key.(*rsa.PublicKey)
	if !ok {
		return nil, nil, loginError("protocol_changed")
	}
	ids := []*string{&c.rid, &c.prid}
	for i, target := range ids {
		value, e := rand.Int(rand.Reader, new(big.Int).Exp(big.NewInt(10), big.NewInt(17), nil))
		if e != nil {
			return nil, nil, logging.Wrap(e, "generate NGA challenge identifier")
		}
		*target = []string{"login", "P"}[i] + fmt.Sprintf("%017d", value)
	}
	image, headers, err := c.request(ctx, "GET", "/login_check_code.php?id="+c.rid+"&from=login", "", nil, 1_000_000)
	if err != nil {
		return nil, nil, err
	}
	if len(image) == 0 || !strings.HasPrefix(headers.Get("Content-Type"), "image/") || !strings.HasPrefix(http.DetectContentType(image), "image/") {
		return nil, nil, loginError("invalid_captcha_image")
	}
	success = true
	return c, image, nil
}
func (c *LoginChallenge) request(ctx context.Context, method, path, contentType string, body io.Reader, limit int64) (data []byte, headers http.Header, err error) {
	ctx, span := logging.Start(ctx, "infrastructure.nga.login_http")
	defer span.End(&err)
	req, err := http.NewRequestWithContext(ctx, method, ngaBaseURL+path, body)
	if err != nil {
		return nil, nil, logging.Wrap(err, "create NGA login request")
	}
	req.Header.Set("User-Agent", c.userAgent)
	req.Header.Set("Referer", ngaBaseURL+"/nuke/account_copy.html?login")
	req.Header.Set("Origin", ngaBaseURL)
	if contentType != "" {
		req.Header.Set("Content-Type", contentType)
	}
	started := time.Now()
	status := 0
	defer func() {
		zerolog.Ctx(ctx).Info().Str("method", method).Str("url", ngaBaseURL+req.URL.Path).Int("status", status).Dur("duration_ms", time.Since(started)).Msg("NGA login HTTP request")
	}()
	response, err := c.client.Do(req)
	if err != nil {
		var ue *url.Error
		if errors.As(err, &ue) {
			err = &url.Error{Op: ue.Op, URL: ngaBaseURL + req.URL.Path, Err: ue.Err}
		}
		return nil, nil, logging.Wrap(err, "request NGA login")
	}
	defer response.Body.Close()
	status = response.StatusCode
	if status != 200 {
		return nil, nil, logging.WithStack(fmt.Errorf("NGA login returned HTTP %d", status))
	}
	data, err = io.ReadAll(io.LimitReader(response.Body, limit+1))
	if err != nil {
		return nil, nil, logging.Wrap(err, "read NGA login response")
	}
	if int64(len(data)) > limit {
		return nil, nil, loginError("response_too_large")
	}
	return data, response.Header, nil
}

var captchaPattern = regexp.MustCompile(`^[a-zA-Z0-9]{6}$`)

func ValidCaptcha(code string) bool { return captchaPattern.MatchString(code) }
func (c *LoginChallenge) Submit(ctx context.Context, name, password, code string) (credentials Credentials, err error) {
	ctx, span := logging.Start(ctx, "infrastructure.nga.submit_login")
	defer span.End(&err)
	if !ValidCaptcha(code) {
		return credentials, loginError("invalid_captcha")
	}
	encrypted, err := rsa.EncryptPKCS1v15(rand.Reader, c.key, []byte(password))
	if err != nil {
		return credentials, logging.Wrap(err, "encrypt NGA login password")
	}
	var body bytes.Buffer
	form := multipart.NewWriter(&body)
	fields := map[string]string{"__lib": "login", "__output": "1", "app_id": "5004", "device": "", "trackid": "", "__act": "login", "__ngaClientChecksum": "", "name": name, "type": loginAccountType(name), "password": base64.StdEncoding.EncodeToString(encrypted), "__inchst": "UTF-8", "rid": c.rid, "captcha": code, "prid": c.prid}
	for k, v := range fields {
		if err = form.WriteField(k, v); err != nil {
			return credentials, logging.Wrap(err, "encode NGA login form")
		}
	}
	if err = form.Close(); err != nil {
		return credentials, logging.Wrap(err, "close NGA login form")
	}
	data, headers, err := c.request(ctx, "POST", "/nuke.php", form.FormDataContentType(), &body, 2_000_000)
	if err != nil {
		return credentials, err
	}
	return parseLoginResponse(data, (&http.Response{Header: headers}).Cookies())
}
func loginAccountType(name string) string {
	if strings.Contains(name, "@") {
		return "mail"
	}
	digits := true
	for _, r := range name {
		if r < '0' || r > '9' {
			digits = false
			break
		}
	}
	if digits && name != "" {
		if len(name) < 10 {
			return "id"
		}
		return "phone"
	}
	compact := strings.NewReplacer(" ", "", "-", "").Replace(name)
	if compact != name && compact != "" {
		if _, err := strconv.ParseUint(compact, 10, 64); err == nil {
			return "phone"
		}
	}
	return ""
}
func parseLoginResponse(body []byte, cookies []*http.Cookie) (Credentials, error) {
	text := strings.TrimSpace(string(body))
	if strings.HasPrefix(text, "window.script_muti_get_var_store=") {
		text = strings.TrimSuffix(strings.TrimSpace(strings.TrimPrefix(text, "window.script_muti_get_var_store=")), ";")
	}
	var data map[string]any
	decoder := json.NewDecoder(strings.NewReader(text))
	decoder.UseNumber()
	if decoder.Decode(&data) != nil {
		return Credentials{}, loginError("protocol_changed")
	}
	// 响应可能含账号/验证码，分类仅返回固定错误码，不传播原始值。
	if value, exists := data["error"]; exists && value != nil {
		raw, _ := json.Marshal(value)
		reason := string(raw)
		if reason != "\"\"" && reason != "[]" && reason != "{}" {
			code := "login_rejected"
			switch {
			case strings.Contains(reason, "手机"), strings.Contains(reason, "短信"), strings.Contains(reason, "腾讯"):
				code = "unsupported_challenge"
			case strings.Contains(reason, "验证码"):
				code = "invalid_captcha"
			case strings.Contains(reason, "密码"), strings.Contains(reason, "用户名"):
				code = "invalid_credentials"
			case strings.Contains(reason, "服务器忙"):
				code = "busy"
			}
			return Credentials{}, loginError(code)
		}
	}
	if code, ok := data["code"].(json.Number); ok && code != "0" {
		return Credentials{}, loginError("login_rejected")
	}
	var candidate any
	switch v := data["data"].(type) {
	case []any:
		if len(v) > 3 {
			candidate = v[3]
		}
	case map[string]any:
		candidate = v["3"]
	}
	budget := 256
	var find func(any, int) (Credentials, bool)
	find = func(v any, depth int) (Credentials, bool) {
		budget--
		if budget < 0 || depth > 8 {
			return Credentials{}, false
		}
		switch v := v.(type) {
		case map[string]any:
			for _, pair := range [][2]string{{"uid", "token"}, {"uid", "cid"}, {"access_uid", "access_token"}, {"ngaPassportUid", "ngaPassportCid"}} {
				uid := ""
				switch x := v[pair[0]].(type) {
				case string:
					uid = x
				case json.Number:
					uid = string(x)
				}
				cid, ok := v[pair[1]].(string)
				if !ok || len(cid) > 4096 {
					continue
				}
				id, e := strconv.ParseUint(uid, 10, 63)
				if e != nil || id == 0 {
					continue
				}
				credential, e := ParseCredentials("", strconv.FormatUint(id, 10), cid)
				if e == nil {
					return credential, true
				}
			}
			for _, child := range v {
				if c, ok := find(child, depth+1); ok {
					return c, true
				}
			}
		case []any:
			for _, child := range v {
				if c, ok := find(child, depth+1); ok {
					return c, true
				}
			}
		}
		return Credentials{}, false
	}
	if c, ok := find(candidate, 0); ok {
		return c, nil
	}
	// 仅采信本次提交响应设置的两项凭据，不使用准备阶段 jar 的旧值。
	uid, cid := "", ""
	for _, c := range cookies {
		switch c.Name {
		case "ngaPassportUid":
			uid = c.Value
		case "ngaPassportCid":
			cid = c.Value
		}
	}
	if uid != "" && cid != "" {
		if c, e := ParseCredentials("", uid, cid); e == nil {
			return c, nil
		}
	}
	return Credentials{}, loginError("candidate_cookie_missing")
}
