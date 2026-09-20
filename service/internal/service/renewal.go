package service

import (
	"context"
	"crypto/rand"
	"encoding/json"
	"errors"
	"fmt"
	"github.com/rs/zerolog"
	"ngareminder/service/internal/infrastructure"
	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/repository"
	"strings"
	"sync"
	"time"
)

type Renewal struct {
	operationMu           sync.Mutex
	operationID, cancelID string
	operationCancel       context.CancelFunc
	monitor               *Monitoring
	work                  sync.Mutex
	challenge             *infrastructure.LoginChallenge
}
type RenewalInput struct {
	Enabled   bool   `json:"enabled" form:"enabled"`
	BindingID int64  `json:"binding_id" form:"binding_id"`
	Name      string `json:"name" form:"name"`
	Password  string `json:"password" form:"password"`
}
type renewalSecret struct{ Name, Password string }
type renewalExpected struct{ UID, AccountHash, AppHash string }
type RenewalOverview struct {
	Settings repository.RenewalSettings `json:"settings"`
	Request  repository.RenewalRequest  `json:"request"`
	Bindings []repository.BotBinding    `json:"bindings"`
}

func (m *Monitoring) Renewal() *Renewal { return m.renewal }
func renewalActive(status string) bool {
	switch status {
	case "awaiting_confirmation", "preparing", "awaiting_captcha", "submitting", "validating_cookie":
		return true
	}
	return false
}
func (r *Renewal) clear() {
	if r.challenge != nil {
		r.challenge.Close()
		r.challenge = nil
	}
}
func (r *Renewal) Close() { r.work.Lock(); defer r.work.Unlock(); r.clear() }
func (r *Renewal) finish(ctx context.Context, v *repository.RenewalRequest, status, message string) (err error) {
	r.clear()
	r.operationMu.Lock()
	cancelled := r.cancelID == v.ID
	r.operationMu.Unlock()
	if cancelled {
		status, message = "cancelled", "已从管理端取消续期"
	}
	v.Status, v.Error, v.Expected = status, message, nil
	write, done := context.WithTimeout(context.WithoutCancel(ctx), 5*time.Second)
	defer done()
	zerolog.Ctx(ctx).Info().Int("account_id", 1).Str("request_id", v.ID).Str("status", status).Msg("Renewal finished")
	return r.monitor.store.SaveRenewal(write, v)
}
func (r *Renewal) latest(ctx context.Context, now time.Time) (v repository.RenewalRequest, err error) {
	v, err = r.monitor.store.LatestRenewal(ctx)
	if err == nil && renewalActive(v.Status) && !now.Before(v.ExpiresAt) {
		err = r.finish(ctx, &v, "expired", "续期交互已过期，请重新发起")
	}
	return
}

// Expire 只处理已过期的交互，错误在此入口记录，循环不重复打印。
func (r *Renewal) Expire(ctx context.Context, now time.Time) (err error) {
	var span *logging.Span
	defer func() {
		panicValue := recover()
		if panicValue != nil {
			err = logging.FromPanic(panicValue)
		}
		if err != nil {
			if span == nil {
				ctx, span = logging.Start(ctx, "service.expire_renewal")
			}
			logging.Error(ctx, err, "Expire renewal failed", zerolog.ErrorLevel)
		}
		if span != nil {
			span.End(&err)
		}
		if panicValue != nil {
			panic(err)
		}
	}()
	if !r.work.TryLock() {
		return nil
	}
	defer r.work.Unlock()
	v, err := r.monitor.store.LatestRenewal(logging.Quiet(ctx))
	if err != nil || !renewalActive(v.Status) || now.Before(v.ExpiresAt) {
		return err
	}
	ctx, span = logging.Start(ctx, "service.expire_renewal")
	zerolog.Ctx(ctx).Info().Str("request_id", v.ID).Time("expires_at", v.ExpiresAt).Msg("Expiring renewal")
	return r.finish(ctx, &v, "expired", "续期交互已过期，请重新发起")
}
func (r *Renewal) Overview(ctx context.Context) (v RenewalOverview, err error) {
	ctx, span := logging.Start(ctx, "service.renewal_overview")
	defer span.End(&err)
	r.work.Lock()
	defer r.work.Unlock()
	v.Settings, err = r.monitor.store.RenewalSettings(ctx)
	if err != nil {
		return v, err
	}
	v.Request, err = r.latest(ctx, time.Now())
	if err != nil {
		return v, err
	}
	v.Bindings, err = r.monitor.store.Bindings(ctx)
	return
}
func (r *Renewal) SaveSettings(ctx context.Context, input RenewalInput) (v repository.RenewalSettings, err error) {
	ctx, span := logging.Start(ctx, "service.save_renewal_settings")
	defer span.End(&err)
	r.work.Lock()
	defer r.work.Unlock()
	v, err = r.monitor.store.RenewalSettings(ctx)
	if err != nil {
		return v, err
	}
	secret := renewalSecret{}
	if len(v.Secret) > 0 {
		raw, e := r.monitor.cipher.Open(ctx, "renewal-secret-v1", v.Secret)
		if e != nil {
			return v, e
		}
		if e = json.Unmarshal(raw, &secret); e != nil {
			return v, logging.Wrap(e, "decode renewal credentials")
		}
	}
	if input.Name != "" {
		secret.Name = input.Name
	}
	if input.Password != "" {
		secret.Password = input.Password
	}
	if input.Enabled {
		if secret.Name == "" || secret.Password == "" {
			return v, InvalidInput("请配置 NGA 登录名和密码")
		}
		if _, err = r.monitor.bot.BindingAddress(ctx, input.BindingID); err != nil {
			return v, InvalidInput("请选择仍有效的管理员绑定")
		}
	}
	raw, _ := json.Marshal(secret)
	v.Secret, err = r.monitor.cipher.Seal(ctx, "renewal-secret-v1", raw)
	if err != nil {
		return v, err
	}
	v.Enabled, v.BindingID, v.Configured = input.Enabled, input.BindingID, secret.Name != "" && secret.Password != ""
	active, err := r.latest(ctx, time.Now())
	if err != nil {
		return v, err
	}
	if renewalActive(active.Status) {
		if err = r.finish(ctx, &active, "cancelled", "续期配置已更改，请重新发起"); err != nil {
			return v, err
		}
	}
	err = r.monitor.store.SaveRenewalSettings(ctx, &v)
	return
}
func (r *Renewal) Start(ctx context.Context) (v repository.RenewalRequest, err error) {
	ctx, span := logging.Start(ctx, "service.start_renewal")
	defer span.End(&err)
	r.work.Lock()
	defer r.work.Unlock()
	if !r.monitor.enabled {
		return v, logging.WithStack(ErrBackgroundDisabled)
	}
	settings, err := r.monitor.store.RenewalSettings(ctx)
	if err != nil {
		return v, err
	}
	if !settings.Enabled || len(settings.Secret) == 0 {
		return v, InvalidInput("请先启用并配置 Cookie 续期")
	}
	bot, err := r.monitor.store.BotSettings(ctx)
	if err != nil {
		return v, err
	}
	if !bot.Enabled {
		return v, InvalidInput("请先启用飞书 Bot")
	}
	v, err = r.latest(ctx, time.Now())
	if err != nil || renewalActive(v.Status) {
		return v, err
	}
	address, err := r.monitor.bot.BindingAddress(ctx, settings.BindingID)
	if err != nil {
		return v, InvalidInput("续期管理员绑定已失效，请重新选择")
	}
	account, err := r.monitor.store.Account(ctx)
	if err != nil {
		return v, err
	}
	if len(account.Cookie) == 0 {
		return v, InvalidInput("请先保存 NGA Cookie，以确定续期目标账号")
	}
	old, err := r.monitor.cipher.Decrypt(ctx, account.Cookie)
	if err != nil {
		return v, err
	}
	app, err := r.monitor.store.FeishuApp(ctx)
	if err != nil {
		return v, err
	}
	raw, _ := json.Marshal(renewalExpected{old.UID, secretHash(string(account.Cookie)), secretHash(string(app.Secret))})
	expected, err := r.monitor.cipher.Seal(ctx, "renewal-expected-v1", raw)
	if err != nil {
		return v, err
	}
	now := time.Now().UTC()
	v = repository.RenewalRequest{ID: rand.Text(), BindingID: settings.BindingID, Status: "awaiting_confirmation", CreatedAt: now, ExpiresAt: now.Add(10 * time.Minute), Expected: expected}
	if err = r.monitor.store.SaveRenewal(ctx, &v); err != nil {
		return v, err
	}
	zerolog.Ctx(ctx).Info().Int("account_id", 1).Str("request_id", v.ID).Msg("Renewal confirmation requested")
	task, done := r.operation(ctx, v.ID)
	defer done()
	err = r.send(task, address, "text", map[string]string{"text": fmt.Sprintf("请确认 NGA Cookie 续期，十分钟内有效。\n/login confirm %s\n取消：/login cancel %s", v.ID, v.ID)}, "renewal-"+v.ID)
	if err != nil {
		err = errors.Join(err, r.finish(ctx, &v, "failed", "续期确认发送失败，请检查飞书配置后重新发起"))
	}
	return
}
func (r *Renewal) send(ctx context.Context, address BotAddress, kind string, body any, uuid string) error {
	app, err := r.monitor.notifications.AppCredentials(ctx)
	if err != nil {
		return err
	}
	return r.monitor.notifications.sender.SendMessage(ctx, app, "chat_id", address.ChatID, kind, body, uuid)
}
func (r *Renewal) Command(ctx context.Context, cmd BotCommand) (reply string, err error) {
	ctx, span := logging.Start(ctx, "service.renewal_command")
	defer span.End(&err)
	r.work.Lock()
	defer r.work.Unlock()
	if cmd.ChatType != "p2p" {
		return "续期命令只允许指定管理员私聊使用", nil
	}
	settings, err := r.monitor.store.RenewalSettings(ctx)
	if err != nil {
		return "", err
	}
	if !settings.Enabled {
		return "Cookie 续期已关闭", nil
	}
	address, err := r.monitor.bot.BindingAddress(ctx, settings.BindingID)
	if err != nil {
		return "", InvalidInput("续期管理员绑定已失效")
	}
	if address.ActorID != cmd.ActorID || address.ChatID != cmd.ChatID {
		return "仅指定的续期管理员可在原绑定私聊中操作", nil
	}
	v, err := r.latest(ctx, time.Now())
	if err != nil {
		return "", err
	}
	if len(cmd.Args) == 1 && cmd.Args[0] == "status" {
		if v.ID == "" {
			return "尚无续期记录，请在管理页发起", nil
		}
		return fmt.Sprintf("续期 %s：%s\n%s", v.ID, StatusText(v.Status), v.Error), nil
	}
	if len(cmd.Args) < 2 {
		return "用法：/login status、/login confirm <request_id>、/login captcha <request_id> <code>、/login cancel <request_id>", nil
	}
	if cmd.Args[1] != v.ID || !renewalActive(v.Status) {
		return "续期交互不存在或已结束，请在管理页重新发起", nil
	}
	if v.BindingID != settings.BindingID {
		return "续期管理员已变更，请重新发起", nil
	}
	if cmd.Args[0] == "cancel" && len(cmd.Args) == 2 {
		return "续期已取消，原 Cookie 保持不变", r.finish(ctx, &v, "cancelled", "")
	}
	if cmd.Args[0] != "confirm" && cmd.Args[0] != "captcha" {
		return "未知续期操作，发送 /help 查看用法", nil
	}
	// 交互期限覆盖网络调用；不会在十分钟后提交或保存候选凭据。
	operation, done := r.operation(ctx, v.ID)
	defer done()
	task, cancel := context.WithDeadline(operation, v.ExpiresAt)
	defer cancel()
	stop := context.AfterFunc(r.monitor.ctx, cancel)
	defer stop()
	defer func() {
		if err != nil {
			status := "failed"
			if !time.Now().Before(v.ExpiresAt) {
				status = "expired"
			}
			err = errors.Join(err, r.finish(ctx, &v, status, FailureMessage(err)))
		}
	}()
	raw, e := r.monitor.cipher.Open(task, "renewal-expected-v1", v.Expected)
	if e != nil {
		return "", e
	}
	var expected renewalExpected
	if e = json.Unmarshal(raw, &expected); e != nil {
		return "", logging.Wrap(e, "decode renewal expected identity")
	}
	appRecord, e := r.monitor.store.FeishuApp(task)
	if e != nil {
		return "", e
	}
	account, e := r.monitor.store.Account(task)
	if e != nil {
		return "", e
	}
	if secretHash(string(appRecord.Secret)) != expected.AppHash || secretHash(string(account.Cookie)) != expected.AccountHash {
		return "", InvalidInput("应用或 NGA Cookie 已更改，请重新发起续期")
	}
	switch cmd.Args[0] {
	case "confirm":
		if len(cmd.Args) != 2 || v.Status != "awaiting_confirmation" {
			return "当前步骤不能确认，请使用 /login status 查看进度", nil
		}
		v.Status = "preparing"
		if err = r.monitor.store.SaveRenewal(task, &v); err != nil {
			return "", err
		}
		var image []byte
		r.challenge, image, err = r.monitor.nga.PrepareLogin(task)
		if err != nil {
			return "", err
		}
		app, e := r.monitor.notifications.AppCredentials(task)
		if e != nil {
			return "", e
		}
		key, e := r.monitor.notifications.sender.UploadImage(task, app, image)
		if e != nil {
			return "", e
		}
		if e = r.send(task, address, "image", map[string]string{"image_key": key}, "captcha-"+v.ID); e != nil {
			return "", e
		}
		v.Status = "awaiting_captcha"
		if err = r.monitor.store.SaveRenewal(task, &v); err != nil {
			return "", err
		}
		return "验证码图片已发送，请回复：/login captcha " + v.ID + " <6 位验证码>", nil
	case "captcha":
		if len(cmd.Args) != 3 || !infrastructure.ValidCaptcha(cmd.Args[2]) {
			return "用法：/login captcha <request_id> <6 位英文字母或数字>", nil
		}
		if v.Status != "awaiting_captcha" || r.challenge == nil {
			return "尚未等待验证码，请先确认登录或重新发起", nil
		}
		if err = r.monitor.lock(); err != nil {
			return "", err
		}
		defer r.monitor.work.Unlock()
		// 与手动 Cookie 变更共用串行锁，锁内再次核对，避免覆盖期间的新凭据。
		account, err = r.monitor.store.Account(task)
		if err != nil {
			return "", err
		}
		if secretHash(string(account.Cookie)) != expected.AccountHash {
			return "", InvalidInput("NGA Cookie 已更改，请重新发起续期")
		}
		v.Status = "submitting"
		if err = r.monitor.store.SaveRenewal(task, &v); err != nil {
			return "", err
		}
		secretRaw, e := r.monitor.cipher.Open(task, "renewal-secret-v1", settings.Secret)
		if e != nil {
			return "", e
		}
		var secret renewalSecret
		if e = json.Unmarshal(secretRaw, &secret); e != nil {
			return "", logging.Wrap(e, "decode renewal credentials")
		}
		candidate, e := r.challenge.Submit(task, secret.Name, secret.Password, cmd.Args[2])
		if e != nil {
			return "", e
		}
		v.Status = "validating_cookie"
		if err = r.monitor.store.SaveRenewal(task, &v); err != nil {
			return "", err
		}
		if candidate.UID != expected.UID {
			return "", InvalidInput("候选 Cookie 的 NGA UID 与原账号不一致，未替换")
		}
		if err = r.monitor.nga.CheckCredentials(task, candidate); err != nil {
			return "", err
		}
		account.Cookie, err = r.monitor.cipher.Encrypt(task, candidate.Cookie)
		if err != nil {
			return "", err
		}
		now := time.Now().UTC()
		account.Status, account.LastError, account.CheckedAt, account.FullCookie = "valid", "", &now, candidate.FullCookie
		v.Status, v.Error, v.Expected = "success", "", nil
		err = r.monitor.store.Transaction(task, func(ctx context.Context, tx *repository.Store) error {
			if e := tx.SaveAccount(ctx, &account); e != nil {
				return e
			}
			if e := tx.ResolveAuthAlert(ctx); e != nil {
				return e
			}
			if e := tx.SetAuthPaused(ctx, false); e != nil {
				return e
			}
			return tx.SaveRenewal(ctx, &v)
		})
		if err != nil {
			return "", err
		}
		r.clear()
		zerolog.Ctx(ctx).Info().Int("account_id", 1).Str("request_id", v.ID).Msg("Renewal succeeded; authentication pause cleared")
		return "Cookie 续期成功，已解除认证暂停；手动暂停的监控保持不变", nil
	}
	return "未知续期操作", nil
}

// 只由“可用→明确认证失败”的状态变化触发；不循环提交密码或重发确认。
func (m *Monitoring) authRenewal(ctx context.Context) {
	settings, err := m.store.RenewalSettings(ctx)
	if err == nil && settings.Enabled {
		_, err = m.renewal.Start(ctx)
	}
	if err != nil {
		logging.Error(ctx, err, "Start authentication renewal failed", zerolog.ErrorLevel)
	}
}

// 取消可中断持锁的外部请求，随后等待该交互收尾；候选 Cookie 的事务使用同一个可取消 context。
func (r *Renewal) operation(ctx context.Context, id string) (context.Context, func()) {
	task, cancel := context.WithCancel(ctx)
	r.operationMu.Lock()
	r.operationID, r.operationCancel = id, cancel
	if r.cancelID == id {
		cancel()
	}
	r.operationMu.Unlock()
	return task, func() {
		cancel()
		r.operationMu.Lock()
		r.operationID, r.operationCancel = "", nil
		r.operationMu.Unlock()
	}
}

func (r *Renewal) Cancel(ctx context.Context) (err error) {
	ctx, span := logging.Start(ctx, "service.cancel_renewal")
	defer span.End(&err)
	account, err := r.monitor.store.Account(ctx)
	if err != nil {
		return err
	}
	if len(account.Cookie) == 0 {
		return InvalidInput("请先配置 NGA 账号")
	}
	active, err := r.monitor.store.LatestRenewal(ctx)
	if err != nil || !renewalActive(active.Status) {
		return err
	}
	r.operationMu.Lock()
	r.cancelID = active.ID
	if r.operationID == active.ID && r.operationCancel != nil {
		r.operationCancel()
	}
	r.operationMu.Unlock()
	r.work.Lock()
	defer r.work.Unlock()
	current, err := r.monitor.store.LatestRenewal(ctx)
	if err != nil {
		return err
	}
	if current.ID != active.ID || current.Status == "success" {
		return nil
	}
	return r.finish(ctx, &current, "cancelled", "已从管理端取消续期")
}

type RenewalTestResult struct {
	OK     bool   `json:"ok"`
	Detail string `json:"detail"`
}

func (r *Renewal) Test(ctx context.Context) (result RenewalTestResult, err error) {
	ctx, span := logging.Start(ctx, "service.test_renewal")
	defer span.End(&err)
	if !r.monitor.enabled {
		return result, logging.WithStack(ErrBackgroundDisabled)
	}
	account, err := r.monitor.store.Account(ctx)
	if err != nil {
		return result, err
	}
	if len(account.Cookie) == 0 {
		return result, InvalidInput("请先配置 NGA 账号")
	}
	settings, err := r.monitor.store.RenewalSettings(ctx)
	if err != nil {
		return result, err
	}
	if len(settings.Secret) == 0 {
		return result, InvalidInput("请先保存续期登录名和密码")
	}
	raw, err := r.monitor.cipher.Open(ctx, "renewal-secret-v1", settings.Secret)
	if err != nil {
		return result, err
	}
	var secret renewalSecret
	if err = json.Unmarshal(raw, &secret); err != nil {
		return result, logging.Wrap(err, "decode renewal credentials")
	}
	if strings.TrimSpace(secret.Name) == "" || secret.Password == "" {
		return result, InvalidInput("请先保存有效的续期登录名和密码")
	}
	bot, err := r.monitor.store.BotSettings(ctx)
	if err != nil {
		return result, err
	}
	if !bot.Enabled {
		return result, InvalidInput("请先启用飞书 Bot")
	}
	if _, err = r.monitor.bot.BindingAddress(ctx, settings.BindingID); err != nil {
		return result, InvalidInput("续期管理员绑定已失效，请重新选择")
	}
	if _, err = r.monitor.notifications.AppCredentials(ctx); err != nil {
		return result, err
	}
	task, cancel := context.WithCancel(ctx)
	defer cancel()
	stop := context.AfterFunc(r.monitor.ctx, cancel)
	defer stop()
	challenge, _, err := r.monitor.nga.PrepareLogin(task)
	if err != nil {
		return result, err
	}
	challenge.Close()
	return RenewalTestResult{OK: true, Detail: "协议正常（RSA 公钥与验证码可用）"}, nil
}
