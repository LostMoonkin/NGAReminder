package infrastructure

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
	"sync"
	"time"

	"github.com/rs/zerolog"

	"ngareminder/service/internal/logging"
)

var (
	ErrNGAAuth              = errors.New("NGA 凭据无效，请替换 Cookie 后重新校验")
	ErrNGAMissing           = errors.New("NGA 主题不存在，已停止自动采集")
	ErrNGAPending           = errors.New("NGA 主题待审核，本轮跳过")
	ErrNGABusy              = errors.New("NGA 服务器忙，请稍后重试")
	ErrNGASearchUnavailable = errors.New("NGA 用户查询暂不可用（空 HTTP 503）")
)

const ngaBaseURL = "https://bbs.nga.cn"

type NGA struct {
	http        *http.Client
	userAgent   string
	gate        sync.Mutex
	lastRequest time.Time
	interval    time.Duration
}

// transport 只用于隔离外部协议的 fixture 验证；生产使用有超时的标准 HTTP Client。
func NewNGA(userAgent string, transport http.RoundTripper) *NGA {
	if transport == nil {
		t := http.DefaultTransport.(*http.Transport).Clone()
		// 沿用旧服务对 NGA/CDN 复用连接后误报认证失效的处理。
		t.DisableKeepAlives = true
		transport = t
	}
	return &NGA{http: &http.Client{Transport: transport, Timeout: 15 * time.Second,
		CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }},
		userAgent: userAgent, interval: 500 * time.Millisecond}
}

func (n *NGA) Close() { n.http.CloseIdleConnections() }

func (n *NGA) CheckCredentials(ctx context.Context, credentials Credentials) (err error) {
	ctx, span := logging.Start(ctx, "infrastructure.nga.check_credentials")
	defer span.End(&err)
	_, err = n.userRequest(ctx, credentials, credentials.UID, true, 1)
	return err
}

func (n *NGA) ThreadPage(ctx context.Context, credentials Credentials, tid int64, page int) (result ThreadPage, err error) {
	ctx, span := logging.Start(ctx, "infrastructure.nga.thread_page")
	defer span.End(&err)
	zerolog.Ctx(ctx).Info().Int64("tid", tid).Int("page", page).Msg("Fetching NGA thread page")
	form := url.Values{"tid": {fmt.Sprint(tid)}, "page": {fmt.Sprint(page)}}
	body, err := n.request(ctx, http.MethodPost, "/app_api.php?__lib=post&__act=list", form.Encode(), credentials.Cookie)
	if err != nil {
		return result, err
	}
	return parseThreadPage(body, tid, page)
}

func (n *NGA) request(ctx context.Context, method, path, form, cookie string) (body []byte, err error) {
	ctx, span := logging.Start(ctx, "infrastructure.nga.http")
	defer span.End(&err)
	n.gate.Lock()
	defer n.gate.Unlock()
	if err = wait(ctx, time.Until(n.lastRequest.Add(n.interval))); err != nil {
		return nil, err
	}
	n.lastRequest = time.Now()
	req, err := http.NewRequestWithContext(ctx, method, ngaBaseURL+path, strings.NewReader(form))
	if err != nil {
		return nil, logging.Wrap(err, "create NGA request")
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	req.Header.Set("User-Agent", n.userAgent)
	req.Header.Set("Accept", "application/json, text/javascript, */*; q=0.01")
	req.Header.Set("Accept-Language", "en-US,en;q=0.9,zh-CN;q=0.8,zh;q=0.7")
	req.Header.Set("Cookie", cookie)
	req.Header.Set("Origin", ngaBaseURL)
	req.Header.Set("Referer", ngaBaseURL+"/")
	started := time.Now()
	status := 0
	defer func() {
		zerolog.Ctx(ctx).Info().Str("method", method).Str("url", ngaBaseURL+req.URL.Path).
			Int("status", status).Dur("duration_ms", time.Since(started)).Msg("NGA HTTP request")
	}()
	response, err := n.http.Do(req)
	if err != nil {
		var requestError *url.Error
		if errors.As(err, &requestError) {
			err = &url.Error{Op: requestError.Op, URL: ngaBaseURL + req.URL.Path, Err: requestError.Err}
		}
		return nil, logging.Wrap(err, "request NGA")
	}
	defer response.Body.Close()
	status = response.StatusCode
	body, err = io.ReadAll(response.Body)
	if err != nil {
		return nil, logging.Wrap(err, "read NGA response")
	}
	if status == http.StatusServiceUnavailable && len(body) == 0 && req.URL.Path == "/thread.php" {
		return nil, logging.WithStack(ErrNGASearchUnavailable)
	}
	if status != http.StatusOK {
		return nil, logging.WithStack(fmt.Errorf("NGA returned HTTP %d", status))
	}
	var envelope struct {
		Code    *number `json:"code"`
		Message string  `json:"msg"`
	}
	if err = json.Unmarshal(body, &envelope); err != nil {
		return nil, logging.Wrap(err, "decode NGA response")
	}
	if envelope.Code == nil {
		return nil, logging.WithStack(errors.New("NGA response is missing its business code"))
	}
	switch code := int64(*envelope.Code); code {
	case 0:
		return body, nil
	case 14:
		err = ErrNGAMissing
	case 46:
		err = ErrNGAAuth
	case 51:
		err = ErrNGAPending
	case 2048:
		// 同一个 2048 业务码需要根据原始消息区分认证失效与服务器繁忙。
		switch {
		case strings.Contains(envelope.Message, "必须登录"), strings.Contains(envelope.Message, "请登录"):
			err = ErrNGAAuth
		case strings.Contains(envelope.Message, "服务器忙"):
			err = ErrNGABusy
		default:
			err = fmt.Errorf("NGA returned business error %d", code)
		}
	default:
		err = fmt.Errorf("NGA returned business error %d", code)
	}
	// 上游 msg 可能含用户输入，不把完整响应或消息放入错误/日志。
	return nil, logging.WithStack(err)
}

func wait(ctx context.Context, delay time.Duration) error {
	if err := ctx.Err(); err != nil {
		return logging.WithStack(err)
	}
	if delay <= 0 {
		return nil
	}
	timer := time.NewTimer(delay)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return logging.WithStack(ctx.Err())
	case <-timer.C:
		return nil
	}
}
