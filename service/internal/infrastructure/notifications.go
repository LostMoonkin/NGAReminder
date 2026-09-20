package infrastructure

import (
	"bytes"
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

	lark "github.com/larksuite/oapi-sdk-go/v3"
	larkim "github.com/larksuite/oapi-sdk-go/v3/service/im/v1"
	"github.com/rs/zerolog"
	"ngareminder/service/internal/logging"
)

type AppCredentials struct {
	AppID     string `json:"app_id"`
	AppSecret string `json:"app_secret"`
}
type ChannelTarget struct {
	ServerURL     string `json:"server_url"`
	DeviceKey     string `json:"device_key"`
	Group         string `json:"group"`
	ReceiveID     string `json:"receive_id"`
	ReceiveIDType string `json:"receive_id_type"`
}
type Notice struct {
	Title, Text, URL string
	Images           []string
}
type Notifier struct {
	http   *http.Client
	mu     sync.Mutex
	app    AppCredentials
	client *lark.Client
}

func NewNotifier(transport http.RoundTripper) *Notifier {
	if transport == nil {
		transport = http.DefaultTransport.(*http.Transport).Clone()
	}
	return &Notifier{http: &http.Client{Transport: transport, Timeout: 20 * time.Second, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}}
}
func (n *Notifier) Close() { n.http.CloseIdleConnections() }

// SDK 原生日志会包含消息正文、token 响应等；实际 HTTP 和业务结果由下面的接入边界统一记录。
type sdkQuiet struct{}

func (sdkQuiet) Debug(context.Context, ...interface{}) {}
func (sdkQuiet) Info(context.Context, ...interface{})  {}
func (sdkQuiet) Warn(context.Context, ...interface{})  {}
func (sdkQuiet) Error(context.Context, ...interface{}) {}

func (n *Notifier) Client(app AppCredentials) *lark.Client {
	n.mu.Lock()
	defer n.mu.Unlock()
	if n.client == nil || n.app != app {
		n.app = app
		n.client = lark.NewClient(app.AppID, app.AppSecret, lark.WithHttpClient(n), lark.WithLogger(sdkQuiet{}))
	}
	return n.client
}

func (n *Notifier) Do(req *http.Request) (response *http.Response, err error) {
	ctx, span := logging.Start(req.Context(), "infrastructure.notification_http")
	defer span.End(&err)
	start := time.Now()
	response, err = n.http.Do(req.WithContext(ctx))
	code := 0
	if response != nil {
		code = response.StatusCode
	}
	zerolog.Ctx(ctx).Info().Str("method", req.Method).Str("host", req.URL.Hostname()).Int("status", code).Dur("duration_ms", time.Since(start)).Msg("Notification HTTP request")
	var ue *url.Error
	if errors.As(err, &ue) {
		err = &url.Error{Op: ue.Op, URL: "[redacted]", Err: ue.Err}
	}
	return response, logging.Wrap(err, "notification HTTP request")
}

func (n *Notifier) Send(ctx context.Context, kind string, target ChannelTarget, app AppCredentials, notice Notice, uuid string) (err error) {
	ctx, span := logging.Start(ctx, "infrastructure.send_notification")
	defer span.End(&err)
	if kind == "bark" {
		body, _ := json.Marshal(map[string]any{"device_key": target.DeviceKey, "title": notice.Title, "body": notice.Text, "group": target.Group, "url": notice.URL})
		req, e := http.NewRequestWithContext(ctx, "POST", strings.TrimRight(target.ServerURL, "/")+"/push", bytes.NewReader(body))
		if e != nil {
			return logging.WithStack(errors.New("invalid Bark service URL"))
		}
		req.Header.Set("Content-Type", "application/json")
		response, e := n.Do(req)
		if e != nil {
			return e
		}
		defer response.Body.Close()
		if response.StatusCode != 200 {
			return logging.WithStack(fmt.Errorf("Bark returned HTTP %d", response.StatusCode))
		}
		var result struct {
			Code int `json:"code"`
		}
		if e = json.NewDecoder(io.LimitReader(response.Body, 1<<20)).Decode(&result); e != nil {
			return logging.Wrap(e, "decode Bark result")
		}
		if result.Code != 200 {
			return logging.WithStack(fmt.Errorf("Bark returned business error %d", result.Code))
		}
		return nil
	}
	elements := []any{map[string]any{"tag": "div", "text": map[string]string{"tag": "lark_md", "content": notice.Text}}}
	for index, source := range notice.Images {
		if index >= 3 {
			break
		}
		data, _, e := n.DownloadResource(ctx, source, 10<<20, true)
		if e == nil {
			var key string
			key, e = n.UploadImage(ctx, app, data)
			if e == nil {
				elements = append(elements, map[string]any{"tag": "img", "img_key": key, "alt": map[string]string{"tag": "plain_text", "content": "图片"}})
			}
		}
		if e != nil {
			notice.Text += "\n图片：" + source
			logging.Error(ctx, e, "Notification image unavailable; sending text and source links", zerolog.WarnLevel)
		}
	}
	elements[0] = map[string]any{"tag": "div", "text": map[string]string{"tag": "lark_md", "content": notice.Text}}
	elements = append(elements, map[string]any{"tag": "action", "actions": []any{map[string]any{"tag": "button", "text": map[string]string{"tag": "plain_text", "content": "查看帖子"}, "type": "primary", "url": notice.URL}}})
	card := map[string]any{"header": map[string]any{"template": "blue", "title": map[string]string{"tag": "plain_text", "content": notice.Title}}, "elements": elements}
	return n.SendMessage(ctx, app, target.ReceiveIDType, target.ReceiveID, "interactive", card, uuid)
}

func (n *Notifier) SendMessage(ctx context.Context, app AppCredentials, idType, id, kind string, body any, uuid string) (err error) {
	ctx, span := logging.Start(ctx, "infrastructure.feishu.send_message")
	defer span.End(&err)
	raw, err := json.Marshal(body)
	if err != nil {
		return logging.Wrap(err, "encode Feishu message")
	}
	req := larkim.NewCreateMessageReqBuilder().ReceiveIdType(idType).Body(larkim.NewCreateMessageReqBodyBuilder().ReceiveId(id).MsgType(kind).Content(string(raw)).Uuid(uuid).Build()).Build()
	result, err := n.Client(app).Im.V1.Message.Create(ctx, req)
	if err != nil {
		return logging.Wrap(err, "send Feishu message")
	}
	if !result.Success() {
		return logging.WithStack(fmt.Errorf("Feishu message API returned code %d", result.Code))
	}
	return nil
}
func (n *Notifier) UploadImage(ctx context.Context, app AppCredentials, data []byte) (key string, err error) {
	ctx, span := logging.Start(ctx, "infrastructure.feishu.upload_image")
	defer span.End(&err)
	result, err := n.Client(app).Im.V1.Image.Create(ctx, larkim.NewCreateImageReqBuilder().Body(larkim.NewCreateImageReqBodyBuilder().ImageType("message").Image(bytes.NewReader(data)).Build()).Build())
	if err != nil {
		return "", logging.Wrap(err, "upload Feishu image")
	}
	if !result.Success() {
		return "", logging.WithStack(fmt.Errorf("Feishu image API returned code %d", result.Code))
	}
	if result.Data == nil || result.Data.ImageKey == nil || *result.Data.ImageKey == "" {
		return "", logging.WithStack(errors.New("Feishu image response omitted image_key"))
	}
	return *result.Data.ImageKey, nil
}

func ValidResourceURL(raw string) bool { return validResourceURL(raw, false) }

func validResourceURL(raw string, imageOnly bool) bool {
	u, err := url.Parse(raw)
	if err != nil || u.Scheme != "https" || u.User != nil || u.Port() != "" {
		return false
	}
	switch u.Hostname() {
	case "img.nga.cn", "img.nga.178.com", "img4.nga.178.com":
		return true
	}
	return !imageOnly && (u.Hostname() == "img6.nga.cn" || u.Hostname() == "img7.nga.cn" || u.Hostname() == "img8.nga.cn")
}
func (n *Notifier) DownloadResource(ctx context.Context, source string, limit int64, imageOnly bool) (data []byte, mime string, err error) {
	ctx, span := logging.Start(ctx, "infrastructure.download_resource")
	defer span.End(&err)
	if !validResourceURL(source, imageOnly) {
		return nil, "", logging.WithStack(errors.New("resource URL is not on an approved NGA host"))
	}
	req, err := http.NewRequestWithContext(ctx, "GET", source, nil)
	if err != nil {
		return nil, "", logging.WithStack(errors.New("invalid NGA resource URL"))
	}
	response, err := n.Do(req)
	if err != nil {
		return nil, "", err
	}
	defer response.Body.Close()
	if response.StatusCode < 200 || response.StatusCode >= 300 {
		return nil, "", logging.WithStack(fmt.Errorf("NGA resource returned HTTP %d", response.StatusCode))
	}
	if limit <= 0 || limit == int64(^uint64(0)>>1) || response.ContentLength > limit {
		return nil, "", logging.WithStack(errors.New("NGA resource exceeds the size limit"))
	}
	data, err = io.ReadAll(io.LimitReader(response.Body, limit+1))
	if err != nil {
		return nil, "", logging.Wrap(err, "read NGA resource")
	}
	if int64(len(data)) > limit || imageOnly && len(data) == 0 {
		return nil, "", logging.WithStack(errors.New("NGA resource is empty or exceeds the size limit"))
	}
	mime = response.Header.Get("Content-Type")
	if imageOnly || mime == "" {
		mime = http.DetectContentType(data)
	}
	if imageOnly && !strings.HasPrefix(mime, "image/") {
		return nil, "", logging.WithStack(errors.New("NGA resource is not a supported image"))
	}
	return data, mime, nil
}
