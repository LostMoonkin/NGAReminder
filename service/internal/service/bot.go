package service

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/rs/zerolog"
	"ngareminder/service/internal/infrastructure"
	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/repository"
)

type Bot struct {
	monitor  *Monitoring
	work     sync.Mutex
	receiver infrastructure.FeishuReceiver
	once     sync.Once
	wg       sync.WaitGroup
}

func (m *Monitoring) Bot() *Bot { return m.bot }

type BotOverview struct {
	Settings repository.BotSettings  `json:"settings"`
	Bindings []repository.BotBinding `json:"bindings"`
}

func (b *Bot) Overview(ctx context.Context) (data BotOverview, err error) {
	ctx, span := logging.Start(ctx, "service.bot_overview")
	defer span.End(&err)
	data.Settings, err = b.monitor.store.BotSettings(ctx)
	if err != nil {
		return data, err
	}
	data.Bindings, err = b.monitor.store.Bindings(ctx)
	return data, err
}
func secretHash(text string) string {
	sum := sha256.Sum256([]byte(text))
	return hex.EncodeToString(sum[:])
}
func (b *Bot) SaveSettings(ctx context.Context, enabled bool, groups *string) (settings repository.BotSettings, err error) {
	ctx, span := logging.Start(ctx, "service.save_bot_settings")
	defer span.End(&err)
	b.work.Lock()
	defer b.work.Unlock()
	settings, err = b.monitor.store.BotSettings(ctx)
	if err != nil {
		return settings, err
	}
	if enabled {
		if _, err = b.monitor.notifications.AppCredentials(ctx); err != nil {
			return settings, err
		}
	}
	if groups != nil {
		items := strings.Fields(*groups)
		raw, _ := json.Marshal(items)
		settings.Groups, err = b.monitor.cipher.Seal(ctx, "bot-groups-v1", raw)
		if err != nil {
			return settings, err
		}
		settings.GroupCount = len(items)
	}
	settings.Enabled = enabled
	if !enabled {
		settings.Status = "disabled"
		settings.LastError = ""
	} else {
		settings.Status = "connecting"
	}
	err = b.monitor.store.SaveBotSettings(ctx, &settings)
	return settings, err
}
func (b *Bot) GenerateCode(ctx context.Context) (code string, expires time.Time, err error) {
	ctx, span := logging.Start(ctx, "service.generate_binding_code")
	defer span.End(&err)
	b.work.Lock()
	defer b.work.Unlock()
	settings, err := b.monitor.store.BotSettings(ctx)
	if err != nil {
		return "", expires, err
	}
	if !settings.Enabled {
		return "", expires, InvalidInput("请先启用飞书 Bot")
	}
	code = rand.Text()
	expires = time.Now().Add(10 * time.Minute).UTC()
	settings.CodeHash, settings.CodeExpires = secretHash(code), &expires
	err = b.monitor.store.SaveBotSettings(ctx, &settings)
	return code, expires, err
}
func (b *Bot) Revoke(ctx context.Context, id int64) (err error) {
	ctx, span := logging.Start(ctx, "service.revoke_bot_binding")
	defer span.End(&err)
	b.work.Lock()
	defer b.work.Unlock()
	return b.monitor.store.RevokeBinding(ctx, id)
}

type BotAddress struct {
	ActorID string `json:"actor_id"`
	ChatID  string `json:"chat_id"`
}
type BotCommand struct {
	AppID, MessageID, ActorID, ChatID, ChatType, Name string
	Args                                              []string
}

func (b *Bot) BindingAddress(ctx context.Context, id int64) (address BotAddress, err error) {
	ctx, span := logging.Start(ctx, "service.bot_binding_address")
	defer span.End(&err)
	binding, err := b.monitor.store.Binding(ctx, id)
	if err != nil {
		return address, err
	}
	raw, err := b.monitor.cipher.Open(ctx, "bot-binding-v1", binding.Address)
	if err != nil {
		return address, err
	}
	err = json.Unmarshal(raw, &address)
	return address, logging.Wrap(err, "decode bot binding address")
}
func (b *Bot) Execute(ctx context.Context, cmd BotCommand) (reply string, err error) {
	ctx, span := logging.Start(ctx, "service.bot_command")
	defer span.End(&err)
	b.work.Lock()
	defer b.work.Unlock()
	if !b.monitor.enabled {
		return "", logging.WithStack(ErrBackgroundDisabled)
	}
	settings, err := b.monitor.store.BotSettings(ctx)
	if err != nil {
		return "", err
	}
	if !settings.Enabled {
		return "", nil
	}
	app, err := b.monitor.notifications.AppCredentials(ctx)
	if err != nil {
		return "", err
	}
	if app.AppID != cmd.AppID {
		return "", nil
	}
	if cmd.MessageID == "" || cmd.ActorID == "" || cmd.ChatID == "" {
		return "", InvalidInput("飞书消息缺少身份信息")
	}
	fresh, err := b.monitor.store.ClaimBotMessage(ctx, secretHash(cmd.AppID+":"+cmd.MessageID))
	if err != nil || !fresh {
		return "", err
	}
	// 先持久化接收标记再执行副作用；崩溃中断的命令需用户重新发消息，不重放副作用。
	reply, err = b.execute(ctx, settings, cmd)
	if err != nil {
		reply = FailureMessage(err) + "；关联 ID：" + logging.TraceID(ctx)
	}
	finish, done := context.WithTimeout(context.WithoutCancel(ctx), 5*time.Second)
	defer done()
	saveErr := b.monitor.store.FinishBotMessage(finish, secretHash(cmd.AppID+":"+cmd.MessageID), reply)
	return reply, errors.Join(err, saveErr)
}
func (b *Bot) execute(ctx context.Context, settings repository.BotSettings, cmd BotCommand) (string, error) {
	zerolog.Ctx(ctx).Info().Str("command", cmd.Name).Str("chat_type", cmd.ChatType).Msg("Processing bot command")
	if cmd.Name == "/bind" {
		if cmd.ChatType != "p2p" {
			return "请在私聊中绑定管理员", nil
		}
		if len(cmd.Args) != 1 {
			return "用法：/bind <绑定码>", nil
		}
		if settings.CodeExpires == nil || !time.Now().Before(*settings.CodeExpires) || subtle.ConstantTimeCompare([]byte(settings.CodeHash), []byte(secretHash(cmd.Args[0]))) != 1 {
			return "绑定码无效、已使用或已过期，请在管理页重新生成", nil
		}
		raw, _ := json.Marshal(BotAddress{cmd.ActorID, cmd.ChatID})
		encrypted, err := b.monitor.cipher.Seal(ctx, "bot-binding-v1", raw)
		if err != nil {
			return "", err
		}
		settings.CodeHash, settings.CodeExpires = "", nil
		err = b.monitor.store.Transaction(ctx, func(ctx context.Context, tx *repository.Store) error {
			if e := tx.SaveBinding(ctx, &repository.BotBinding{ActorHash: secretHash(cmd.ActorID), Address: encrypted}); e != nil {
				return e
			}
			return tx.SaveBotSettings(ctx, &settings)
		})
		return "管理员绑定成功，发送 /help 查看命令", err
	}
	bindings, err := b.monitor.store.Bindings(ctx)
	if err != nil {
		return "", err
	}
	bound := false
	for _, binding := range bindings {
		if binding.ActorHash == secretHash(cmd.ActorID) {
			bound = true
			break
		}
	}
	if !bound {
		return "请先在管理页生成绑定码，并私聊发送 /bind <绑定码>", nil
	}
	if cmd.ChatType != "p2p" {
		groups := []string{}
		if len(settings.Groups) > 0 {
			raw, e := b.monitor.cipher.Open(ctx, "bot-groups-v1", settings.Groups)
			if e != nil {
				return "", e
			}
			if e = json.Unmarshal(raw, &groups); e != nil {
				return "", logging.Wrap(e, "decode allowed bot groups")
			}
		}
		if cmd.ChatType != "group" || !slices.Contains(groups, cmd.ChatID) {
			return "此群未获授权，请使用私聊", nil
		}
	}
	switch cmd.Name {
	case "/help":
		return "/help\n/status\n/watch list\n/watch run <watch_id>", nil
	case "/status":
		data, e := b.monitor.Overview(ctx)
		if e != nil {
			return "", e
		}
		notifications, e := b.monitor.notifications.Overview(ctx, 1)
		if e != nil {
			return "", e
		}
		return fmt.Sprintf("NGA 账号：%s\n监控：%d 个\n通知渠道：%d 个\nBot：%s", StatusText(data.Account.Status), len(data.Watches), len(notifications.Channels), StatusText(settings.Status)), nil
	case "/watch":
		if len(cmd.Args) == 1 && strings.EqualFold(cmd.Args[0], "list") {
			data, e := b.monitor.Overview(ctx)
			if e != nil {
				return "", e
			}
			lines := []string{}
			for _, w := range data.Watches {
				target := fmt.Sprintf("TID %d", w.TID)
				if w.Kind == "uid" {
					target = fmt.Sprintf("UID %d", w.UID)
				}
				state := StatusText(w.State)
				if w.Paused {
					state = "已暂停"
				}
				result := "未运行"
				if w.LastRun != nil {
					result = StatusText(w.LastRun.Status)
				}
				lines = append(lines, fmt.Sprintf("#%d %s %s · %s · 最近：%s", w.ID, target, w.Label, state, result))
			}
			if len(lines) == 0 {
				return "尚无监控", nil
			}
			return strings.Join(lines, "\n"), nil
		}
		if len(cmd.Args) == 2 && strings.EqualFold(cmd.Args[0], "run") {
			id, e := strconv.ParseInt(cmd.Args[1], 10, 64)
			if e != nil || id <= 0 {
				return "watch_id 必须为正整数", nil
			}
			run, e := b.monitor.StartRun(ctx, id)
			if e != nil {
				return "", e
			}
			return fmt.Sprintf("已接受监控 #%d，运行 #%d；结果请在管理页查看。", id, run.ID), nil
		}
		return "用法：/watch list 或 /watch run <watch_id>", nil
	default:
		return "未知命令，发送 /help 查看当前支持的命令", nil
	}
}
func (b *Bot) SetReceiver(receiver infrastructure.FeishuReceiver) { b.receiver = receiver }
func (b *Bot) Start() {
	if !b.monitor.enabled || b.receiver == nil {
		return
	}
	b.once.Do(func() { b.wg.Add(1); go b.connections() })
}
func (b *Bot) connections() {
	defer b.wg.Done()
	ticker := time.NewTicker(2 * time.Second)
	defer ticker.Stop()
	var cancel context.CancelFunc
	var done chan struct{}
	var active string
	var retryAt time.Time
	stop := func() {
		if cancel != nil {
			cancel()
			<-done
			cancel = nil
		}
	}
	defer stop()
	for {
		select {
		case <-b.monitor.ctx.Done():
			return
		case now := <-ticker.C:
			ctx, span := logging.Start(b.monitor.log.WithContext(context.Background()), "service.bot_connection_check")
			settings, err := b.monitor.store.BotSettings(ctx)
			var app repository.FeishuApp
			if err == nil {
				app, err = b.monitor.store.FeishuApp(ctx)
			}
			if err != nil {
				logging.Error(ctx, err, "Read bot configuration failed", zerolog.ErrorLevel)
				span.End(&err)
				continue
			}
			key := secretHash(string(app.Secret))
			if !settings.Enabled || len(app.Secret) == 0 {
				stop()
				active = ""
				span.End(nil)
				continue
			}
			if cancel != nil {
				select {
				case <-done:
					cancel = nil
					retryAt = now.Add(30 * time.Second)
				default:
				}
			}
			if active != key {
				stop()
				retryAt = time.Time{}
			}
			if cancel == nil && !now.Before(retryAt) {
				credentials, e := b.monitor.notifications.AppCredentials(ctx)
				if e != nil {
					logging.Error(ctx, e, "Read bot app failed", zerolog.ErrorLevel)
					span.End(&e)
					continue
				}
				active = key
				taskCtx, taskCancel := context.WithCancel(b.monitor.log.WithContext(context.Background()))
				cancel = taskCancel
				done = make(chan struct{})
				go func(done chan struct{}) {
					defer close(done)
					taskCtx, root := logging.Start(taskCtx, "service.bot_connection")
					var err error
					defer root.End(&err)
					update := func(state string, cause error) {
						writeCtx, finish := context.WithTimeout(context.WithoutCancel(taskCtx), 5*time.Second)
						defer finish()
						message := ""
						if cause != nil {
							message = FailureMessage(cause)
							logging.Error(taskCtx, cause, "Feishu bot connection failed", zerolog.ErrorLevel)
						}
						if e := b.monitor.store.BotConnectionState(writeCtx, state, message); e != nil {
							logging.Error(taskCtx, e, "Save bot connection state failed", zerolog.ErrorLevel)
						}
					}
					update("connecting", nil)
					err = b.monitor.notifications.sender.ConnectBot(taskCtx, credentials, b.receiver, func() { update("connected", nil) }, func(e error) { update("reconnecting", e) })
					if taskCtx.Err() == nil && err != nil {
						update("failed", err)
					} else {
						err = nil
					}
				}(done)
			}
			span.End(nil)
		}
	}
}
func (b *Bot) Close() { b.wg.Wait() }

func (b *Bot) Reply(ctx context.Context, cmd BotCommand, text string) (err error) {
	ctx, span := logging.Start(ctx, "service.reply_bot_command")
	defer span.End(&err)
	app, err := b.monitor.notifications.AppCredentials(ctx)
	if err != nil {
		return err
	}
	return b.monitor.notifications.sender.Reply(ctx, app, cmd.MessageID, text, "reply-"+secretHash(cmd.MessageID)[:24])
}
