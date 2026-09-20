package service

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/url"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/rs/zerolog"
	"ngareminder/service/internal/content"
	"ngareminder/service/internal/infrastructure"
	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/repository"
)

type Notifications struct {
	store   *repository.Store
	cipher  *infrastructure.CredentialCipher
	sender  *infrastructure.Notifier
	log     *logging.Logger
	enabled bool
	ctx     context.Context
	work    sync.Mutex
	wg      sync.WaitGroup
	once    sync.Once
}

func (m *Monitoring) Notifications() *Notifications { return m.notifications }

type NotificationOverview struct {
	Alerts   []repository.SystemAlert `json:"alerts"`
	App      FeishuAppInfo            `json:"app"`
	Channels []repository.Channel     `json:"channels"`
	Events   []repository.InboxEvent  `json:"events"`
}

// 管理页可回显应用标识，Secret 和密文不进入展示数据。
type FeishuAppInfo struct {
	Configured bool   `json:"configured"`
	AppID      string `json:"app_id"`
}

func (n *Notifications) Overview(ctx context.Context, page int) (NotificationOverview, error) {
	return n.FilteredOverview(ctx, page, "all")
}

func (n *Notifications) FilteredOverview(ctx context.Context, page int, filter string) (data NotificationOverview, err error) {
	ctx, span := logging.Start(ctx, "service.notification_overview")
	defer span.End(&err)
	switch filter {
	case "all", "unread", "delivery", "alerts":
	default:
		return data, InvalidInput("收件箱筛选类型无效")
	}
	app, err := n.store.FeishuApp(ctx)
	if err != nil {
		return data, err
	}
	if app.Configured {
		raw, e := n.cipher.Open(ctx, "feishu-app-v1", app.Secret)
		if e != nil {
			return data, e
		}
		if err = json.Unmarshal(raw, &data.App); err != nil {
			return data, logging.Wrap(err, "decode Feishu application info")
		}
	}
	data.App.Configured = app.Configured
	if data.Channels, err = n.store.Channels(ctx); err != nil {
		return data, err
	}
	if data.Alerts, err = n.store.Alerts(ctx, page); err != nil {
		return data, err
	}
	data.Events, err = n.store.FilteredInbox(ctx, page, filter)
	return data, err
}
func (n *Notifications) AppCredentials(ctx context.Context) (app infrastructure.AppCredentials, err error) {
	ctx, span := logging.Start(ctx, "service.feishu_credentials")
	defer span.End(&err)
	row, err := n.store.FeishuApp(ctx)
	if err != nil {
		return app, err
	}
	if len(row.Secret) == 0 {
		return app, InvalidInput("请先配置飞书应用")
	}
	raw, err := n.cipher.Open(ctx, "feishu-app-v1", row.Secret)
	if err != nil {
		return app, err
	}
	err = json.Unmarshal(raw, &app)
	return app, logging.Wrap(err, "decode Feishu credentials")
}
func (n *Notifications) SaveApp(ctx context.Context, input infrastructure.AppCredentials) (app repository.FeishuApp, err error) {
	ctx, span := logging.Start(ctx, "service.save_feishu_app")
	defer span.End(&err)
	n.work.Lock()
	defer n.work.Unlock()
	app, err = n.store.FeishuApp(ctx)
	if err != nil {
		return app, err
	}
	if len(app.Secret) > 0 {
		old, e := n.AppCredentials(ctx)
		if e != nil {
			return app, e
		}
		if input.AppID == "" {
			input.AppID = old.AppID
		}
		if input.AppSecret == "" {
			input.AppSecret = old.AppSecret
		}
	}
	if strings.TrimSpace(input.AppID) == "" || strings.TrimSpace(input.AppSecret) == "" {
		return app, InvalidInput("飞书 App ID 与 App Secret 不能为空")
	}
	raw, _ := json.Marshal(input)
	app.Secret, err = n.cipher.Seal(ctx, "feishu-app-v1", raw)
	if err != nil {
		return app, err
	}
	app.Configured = true
	err = n.store.SaveFeishuApp(ctx, &app)
	return app, err
}

type ChannelInput struct {
	Name    string `json:"name" form:"name"`
	Kind    string `json:"kind" form:"kind"`
	Enabled bool   `json:"enabled" form:"enabled"`
	infrastructure.ChannelTarget
}

func (n *Notifications) target(ctx context.Context, c repository.Channel) (target infrastructure.ChannelTarget, err error) {
	raw, err := n.cipher.Open(ctx, "notification-target-v1", c.Secret)
	if err != nil {
		return target, err
	}
	err = json.Unmarshal(raw, &target)
	return target, logging.Wrap(err, "decode notification target")
}
func (n *Notifications) SaveChannel(ctx context.Context, id int64, input ChannelInput) (channel repository.Channel, err error) {
	ctx, span := logging.Start(ctx, "service.save_notification_channel")
	defer span.End(&err)
	n.work.Lock()
	defer n.work.Unlock()
	if strings.TrimSpace(input.Name) == "" || input.Kind != "bark" && input.Kind != "feishu" {
		return channel, InvalidInput("请填写渠道名称并选择 Bark 或飞书")
	}
	target := input.ChannelTarget
	if id != 0 {
		channel, err = n.store.Channel(ctx, id)
		if err != nil {
			return channel, err
		}
		if channel.Kind == input.Kind {
			old, e := n.target(ctx, channel)
			if e != nil {
				return channel, e
			}
			if target.ServerURL == "" {
				target.ServerURL = old.ServerURL
			}
			if target.DeviceKey == "" {
				target.DeviceKey = old.DeviceKey
			}
			if target.ReceiveID == "" {
				target.ReceiveID = old.ReceiveID
			}
			if target.ReceiveIDType == "" {
				target.ReceiveIDType = old.ReceiveIDType
			}
		}
	}
	if input.Kind == "bark" {
		if target.ServerURL == "" {
			target.ServerURL = "https://api.day.app"
		}
		u, e := url.Parse(target.ServerURL)
		if e != nil || u.Host == "" || u.Scheme != "https" && u.Scheme != "http" || u.User != nil || u.RawQuery != "" || u.Fragment != "" || target.DeviceKey == "" {
			return channel, InvalidInput("请填写 Bark 设备 key 和有效的 HTTP/HTTPS 服务地址")
		}
	} else {
		if target.ReceiveIDType == "" {
			target.ReceiveIDType = "chat_id"
		}
		if target.ReceiveID == "" || !slices.Contains([]string{"chat_id", "open_id", "user_id", "union_id", "email"}, target.ReceiveIDType) {
			return channel, InvalidInput("请填写飞书接收 ID 并选择对应类型")
		}
		if _, err = n.AppCredentials(ctx); err != nil {
			return channel, err
		}
	}
	raw, _ := json.Marshal(target)
	channel.Secret, err = n.cipher.Seal(ctx, "notification-target-v1", raw)
	if err != nil {
		return channel, err
	}
	channel.Name, channel.Kind, channel.Enabled = strings.TrimSpace(input.Name), input.Kind, input.Enabled
	zerolog.Ctx(ctx).Info().Int64("channel_id", id).Str("kind", input.Kind).Bool("enabled", input.Enabled).Msg("Saving notification channel")
	err = n.store.Transaction(ctx, func(ctx context.Context, tx *repository.Store) error {
		if e := tx.SaveChannel(ctx, &channel); e != nil {
			return e
		}
		return tx.EnqueueOpenAlerts(ctx)
	})
	return channel, err
}
func (n *Notifications) ChangeChannel(ctx context.Context, id int64, action string) (err error) {
	ctx, span := logging.Start(ctx, "service.change_notification_channel")
	defer span.End(&err)
	n.work.Lock()
	defer n.work.Unlock()
	return n.store.Transaction(ctx, func(ctx context.Context, tx *repository.Store) error {
		c, e := tx.Channel(ctx, id)
		if e != nil {
			return e
		}
		if action == "delete" {
			watches, e := tx.Watches(ctx)
			if e != nil {
				return e
			}
			for _, w := range watches {
				if slices.Contains(w.ChannelIDs, id) {
					return InvalidInput("渠道仍被监控引用，请先在监控配置中解除引用")
				}
			}
			if e = tx.CancelChannelDeliveries(ctx, id); e != nil {
				return e
			}
			return tx.DeleteChannel(ctx, id)
		}
		c.Enabled = action == "enable"
		if e := tx.SaveChannel(ctx, &c); e != nil {
			return e
		}
		return tx.EnqueueOpenAlerts(ctx)
	})
}
func (n *Notifications) TestChannel(ctx context.Context, id int64) (err error) {
	ctx, span := logging.Start(ctx, "service.test_notification_channel")
	defer span.End(&err)
	if !n.enabled {
		return logging.WithStack(ErrBackgroundDisabled)
	}
	n.work.Lock()
	defer n.work.Unlock()
	c, err := n.store.Channel(ctx, id)
	if err != nil {
		return err
	}
	if !c.Enabled {
		return InvalidInput("渠道已关闭，请启用后测试")
	}
	return n.send(ctx, c, infrastructure.Notice{Title: "NGA Reminder 通知测试", Text: "通知渠道连接测试", URL: "https://bbs.nga.cn/"}, "")
}
func (n *Notifications) send(ctx context.Context, c repository.Channel, notice infrastructure.Notice, uuid string) error {
	target, err := n.target(ctx, c)
	if err != nil {
		return err
	}
	var app infrastructure.AppCredentials
	if c.Kind == "feishu" {
		app, err = n.AppCredentials(ctx)
		if err != nil {
			return err
		}
	}
	return n.sender.Send(ctx, c.Kind, target, app, notice, uuid)
}

func validateNotificationWatch(ctx context.Context, tx *repository.Store, w repository.Watch) error {
	for _, uid := range w.AuthorUIDs {
		if uid <= 0 {
			return InvalidInput("作者 UID 必须为正整数")
		}
	}
	for _, id := range w.ChannelIDs {
		if _, err := tx.Channel(ctx, id); err != nil {
			return InvalidInput("所选通知渠道不存在")
		}
	}
	return nil
}

// 观察去重属于 watch；事件与投递按全局内容去重，避免先由另一监控保存时漏通知。
func (n *Notifications) Record(ctx context.Context, tx *repository.Store, w repository.Watch, posts []repository.Post, silent bool) (err error) {
	ctx, span := logging.Start(ctx, "service.match_notifications")
	defer span.End(&err)
	for _, p := range posts {
		saved, e := tx.PostByKey(ctx, p.TID, p.Key)
		if e != nil {
			return e
		}
		fresh, e := tx.ObservePost(ctx, w.ID, saved.ID)
		if e != nil {
			return e
		}
		if !fresh || silent || w.Kind == "uid" && p.AuthorUID != w.UID || w.Kind == "tid" && len(w.AuthorUIDs) > 0 && !slices.Contains(w.AuthorUIDs, p.AuthorUID) {
			continue
		}
		event, e := tx.MatchEvent(ctx, saved.ID, w)
		if e != nil {
			return e
		}
		for _, id := range w.ChannelIDs {
			c, e := tx.Channel(ctx, id)
			if e != nil {
				return e
			}
			if !c.Enabled {
				continue
			}
			if e = tx.Enqueue(ctx, event.ID, c); e != nil {
				return e
			}
		}
		zerolog.Ctx(ctx).Info().Int64("watch_id", w.ID).Int64("event_id", event.ID).Int64("post_id", saved.ID).Msg("New content matched inbox")
	}
	return nil
}
func (n *Notifications) MarkRead(ctx context.Context, id int64, read bool) (err error) {
	ctx, span := logging.Start(ctx, "service.mark_inbox_read")
	defer span.End(&err)
	return n.store.MarkRead(ctx, id, read)
}
func (n *Notifications) Retry(ctx context.Context, id int64) (err error) {
	ctx, span := logging.Start(ctx, "service.retry_notification")
	defer span.End(&err)
	n.work.Lock()
	defer n.work.Unlock()
	d, err := n.store.Delivery(ctx, id)
	if err != nil {
		return err
	}
	c, err := n.store.Channel(ctx, d.ChannelID)
	if err != nil {
		return err
	}
	if !c.Enabled {
		return InvalidInput("渠道已关闭，请先启用")
	}
	if d.AlertID != 0 {
		alert, e := n.store.Alert(ctx, d.AlertID)
		if e != nil {
			return e
		}
		if alert.ResolvedAt != nil {
			return InvalidInput("该告警已解除，无需重新投递")
		}
	}
	if d.Status == "sent" {
		return InvalidInput("该通知已发送成功")
	}
	d.Status, d.Attempts, d.Error, d.NextAttempt = "pending", 0, "", time.Now().UTC()
	return n.store.SaveDelivery(ctx, &d)
}

// Deliver 每轮只发送一条可投递通知，错误在此入口记录，循环不重复打印。
func (n *Notifications) Deliver(ctx context.Context, now time.Time) (err error) {
	var span *logging.Span
	defer func() {
		panicValue := recover()
		if panicValue != nil {
			err = logging.FromPanic(panicValue)
		}
		if err != nil {
			if span == nil {
				ctx, span = logging.Start(ctx, "service.deliver_notifications")
			}
			logging.Error(ctx, err, "Notification worker failed", zerolog.ErrorLevel)
		}
		if span != nil {
			span.End(&err)
		}
		if panicValue != nil {
			panic(err)
		}
	}()
	if !n.enabled {
		return nil
	}
	if !n.work.TryLock() {
		return nil
	}
	defer n.work.Unlock()
	probe := logging.Quiet(ctx)
	pending, err := n.store.PendingDeliveries(probe)
	if err != nil {
		return err
	}
	for _, d := range pending {
		if d.NextAttempt.After(now) {
			continue
		}
		c, e := n.store.Channel(probe, d.ChannelID)
		if errors.Is(e, repository.ErrNotFound) {
			continue
		}
		if e != nil {
			return e
		}
		if !c.Enabled {
			continue
		}
		ctx, span = logging.Start(ctx, "service.deliver_notifications")
		zerolog.Ctx(ctx).Info().Int64("channel_id", c.ID).Int64("delivery_id", d.ID).Int("attempt", d.Attempts+1).Msg("Starting notification delivery")
		if d.AlertID != 0 {
			alert, e := n.store.Alert(ctx, d.AlertID)
			if e != nil {
				return e
			}
			if alert.ResolvedAt != nil {
				d.Status, d.Error = "cancelled", "告警已解除"
				return n.store.SaveDelivery(ctx, &d)
			}
		}
		notice, e := n.deliveryNotice(ctx, d, c.Kind)
		if e != nil {
			return e
		}

		sendErr := n.send(ctx, c, notice, fmt.Sprintf("nga-delivery-%d", d.ID))
		d.Attempts++
		d.TraceID = logging.TraceID(ctx)
		d.Error = ""
		if sendErr == nil {
			d.Status = "sent"
		} else {
			d.Status = "pending"
			if d.Attempts >= 5 {
				d.Status = "failed"
			}
			d.Error = FailureMessage(sendErr)
			d.NextAttempt = now.Add(time.Duration(d.Attempts*d.Attempts) * time.Minute).UTC()
			logging.Error(ctx, sendErr, "Notification delivery failed", zerolog.WarnLevel)
		}
		zerolog.Ctx(ctx).Info().Int64("channel_id", c.ID).Int64("delivery_id", d.ID).Int("attempt", d.Attempts).Str("status", d.Status).Msg("Notification delivery finished")
		finish, done := context.WithTimeout(context.WithoutCancel(ctx), 5*time.Second)
		e = n.store.SaveDelivery(finish, &d)
		done()
		if e != nil {
			return e
		}
		return nil
	}
	return nil
}
func (n *Notifications) Start() {
	if !n.enabled {
		return
	}
	n.once.Do(func() {
		n.wg.Add(1)
		go func() {
			defer n.wg.Done()
			ticker := time.NewTicker(time.Second)
			defer ticker.Stop()
			for {
				select {
				case <-n.ctx.Done():
					return
				case now := <-ticker.C:
					ctx, cancel := context.WithCancel(n.log.WithContext(context.Background()))
					stop := context.AfterFunc(n.ctx, cancel)
					_ = n.Deliver(ctx, now)
					stop()
					cancel()
				}
			}
		}()
	})
}
func (n *Notifications) Close() { n.wg.Wait(); n.work.Lock(); defer n.work.Unlock(); n.sender.Close() }

func (n *Notifications) deliveryNotice(ctx context.Context, d repository.Delivery, kind string) (infrastructure.Notice, error) {
	if d.AlertID != 0 {
		alert, err := n.store.Alert(ctx, d.AlertID)
		if err != nil {
			return infrastructure.Notice{}, err
		}
		return infrastructure.Notice{Title: alert.Title, Text: alert.Body, URL: alert.URL}, nil
	}
	event, err := n.store.Event(ctx, d.EventID)
	if err != nil {
		return infrastructure.Notice{}, err
	}
	p := event.Post
	thread, err := n.store.Thread(ctx, p.TID)
	if err != nil && !errors.Is(err, repository.ErrNotFound) {
		return infrastructure.Notice{}, err
	}
	title := thread.Title
	if kind != "feishu" {
		title = content.Summary(title, 80)
		if title == "" {
			title = fmt.Sprintf("NGA TID %d", p.TID)
		}
	}
	heading := fmt.Sprintf("%s · #%d", p.Author, p.Floor)
	for _, source := range event.Sources {
		if source.Kind != "uid" {
			continue
		}
		name := p.Author
		if strings.TrimSpace(name) == "" {
			name = fmt.Sprintf("UID %d", p.AuthorUID)
		}
		title = "用户监控：" + name
		break
	}
	if kind == "feishu" {
		// 飞书接收原始 NGA 正文，由独立 compact Markdown 渲染器统一处理格式、图片与长度。
		link := fmt.Sprintf("https://bbs.nga.cn/read.php?tid=%d", p.TID)
		if p.PID > 0 {
			link += fmt.Sprintf("&pid=%d", p.PID)
		}
		return infrastructure.Notice{Title: title, Text: heading + "\n\n" + p.Body, URL: link}, nil
	}
	body := content.Summary(p.Body, 1500)
	text := heading + "\n" + body + "\n" + p.SourceURL
	return infrastructure.Notice{Title: title, Text: text, URL: p.SourceURL}, nil
}
