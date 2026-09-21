package service

import (
	"context"
	"errors"
	"strings"
	"sync"
	"time"

	"github.com/rs/zerolog"

	"ngareminder/service/internal/config"
	"ngareminder/service/internal/infrastructure"
	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/repository"
)

var (
	ErrPaused             = errors.New("监控已手动暂停，请恢复后再运行")
	ErrBusy               = errors.New("已有采集或账号操作正在执行，请完成后再试")
	ErrBackgroundDisabled = errors.New("后台任务已关闭，核验模式不访问 NGA")
)

type InputError struct{ Message string }

func (e *InputError) Error() string     { return e.Message }
func InvalidInput(message string) error { return logging.WithStack(&InputError{message}) }

type Monitoring struct {
	storeRaw      bool
	store         *repository.Store
	nga           *infrastructure.NGA
	cipher        *infrastructure.CredentialCipher
	log           *logging.Logger
	enabled       bool
	ctx           context.Context
	cancel        context.CancelFunc
	location      *time.Location
	scheduler     sync.WaitGroup
	schedulerOnce sync.Once
	notifications *Notifications
	bot           *Bot
	renewal       *Renewal
	resources     *Resources
	// 账号替换和资源清理独占；不同 watch 共享，只对同一 watch 的运行/修改互斥。
	work          sync.RWMutex
	watchWork     sync.Mutex
	activeWatches map[int64]bool
	backfillWork  sync.Mutex
}

func NewMonitoring(ctx context.Context, cfg config.Config, store *repository.Store, nga *infrastructure.NGA, log *logging.Logger, sender *infrastructure.Notifier) (monitor *Monitoring, err error) {
	initCtx, span := logging.Start(ctx, "service.initialize_monitoring")
	defer span.End(&err)
	cipher, err := infrastructure.NewCredentialCipher(cfg.EncryptionKey)
	if err != nil {
		return nil, err
	}
	if err = store.InterruptBackfills(initCtx); err != nil {
		return nil, err
	}
	if err = store.InterruptRuns(initCtx); err != nil {
		return nil, err
	}
	location, err := time.LoadLocation(cfg.Timezone)
	if err != nil {
		return nil, logging.Wrap(err, "load monitoring timezone")
	}
	lifeCtx, cancel := context.WithCancel(ctx)
	notices := &Notifications{store: store, cipher: cipher, sender: sender, log: log, enabled: cfg.BackgroundEnabled, ctx: lifeCtx}
	monitor = &Monitoring{storeRaw: cfg.StoreRawPayload, notifications: notices, store: store, nga: nga, cipher: cipher, log: log, enabled: cfg.BackgroundEnabled, ctx: lifeCtx, cancel: cancel, location: location}
	monitor.bot = &Bot{monitor: monitor}
	monitor.renewal = &Renewal{monitor: monitor}
	monitor.resources = &Resources{monitor: monitor, files: &infrastructure.Assets{Path: cfg.AssetsPath}, databasePath: cfg.DatabasePath, maxDownloadBytes: cfg.MaxDownloadBytes}
	if err = store.InterruptRenewals(initCtx); err != nil {
		cancel()
		return nil, err
	}
	if cfg.BackgroundEnabled {
		account, e := store.Account(initCtx)
		if e == nil && account.Status == "auth_paused" {
			e = store.Transaction(ctx, func(ctx context.Context, tx *repository.Store) error { return tx.EnsureAuthAlert(ctx) })
		}
		if e != nil {
			cancel()
			return nil, e
		}
	}
	return monitor, nil
}

func (m *Monitoring) Close() {
	_, span := logging.Start(m.log.WithContext(m.ctx), "service.stop_monitoring")
	defer span.End(nil)
	m.cancel()
	m.scheduler.Wait()
	m.bot.Close()
	m.renewal.Close()
	m.notifications.Close()
	m.work.Lock()
	defer m.work.Unlock()
	m.resources.work.Lock()
	defer m.resources.work.Unlock()
	m.nga.Close()
}

func (m *Monitoring) lock() error {
	if !m.work.TryLock() {
		return logging.WithStack(ErrBusy)
	}
	if err := m.ctx.Err(); err != nil {
		m.work.Unlock()
		return logging.WithStack(err)
	}
	return nil
}

func (m *Monitoring) lockWatch(id int64) error {
	if !m.work.TryRLock() {
		return logging.WithStack(ErrBusy)
	}
	if err := m.ctx.Err(); err != nil {
		m.work.RUnlock()
		return logging.WithStack(err)
	}
	m.watchWork.Lock()
	defer m.watchWork.Unlock()
	if m.activeWatches[id] {
		m.work.RUnlock()
		return logging.WithStack(ErrBusy)
	}
	if m.activeWatches == nil {
		m.activeWatches = make(map[int64]bool)
	}
	m.activeWatches[id] = true
	return nil
}

func (m *Monitoring) unlockWatch(id int64) {
	m.watchWork.Lock()
	delete(m.activeWatches, id)
	m.watchWork.Unlock()
	m.work.RUnlock()
}

type Overview struct {
	Account repository.Account         `json:"account"`
	Watches []repository.Watch         `json:"watches"`
	Threads []repository.ThreadSummary `json:"threads"`
}

func (m *Monitoring) Overview(ctx context.Context) (data Overview, err error) {
	ctx, span := logging.Start(ctx, "service.monitoring_overview")
	defer span.End(&err)
	if data.Account, err = m.store.Account(ctx); err != nil {
		return data, err
	}
	if data.Watches, err = m.store.Watches(ctx); err != nil {
		return data, err
	}
	latest, err := m.store.LatestRuns(ctx)
	if err != nil {
		return data, err
	}
	byWatch := make(map[int64]*repository.Run, len(latest))
	for i := range latest {
		byWatch[latest[i].WatchID] = &latest[i]
	}
	for i := range data.Watches {
		m.describeWatch(&data.Watches[i], time.Now())
		data.Watches[i].LastRun = byWatch[data.Watches[i].ID]
	}
	data.Threads, err = m.store.Threads(ctx)
	return data, err
}

func (m *Monitoring) Account(ctx context.Context) (account repository.Account, err error) {
	ctx, span := logging.Start(ctx, "service.nga_account")
	defer span.End(&err)
	return m.store.Account(ctx)
}

type AccountInput struct {
	Cookie      string `json:"cookie" form:"cookie"`
	PassportUID string `json:"passport_uid" form:"passport_uid"`
	PassportCID string `json:"passport_cid" form:"passport_cid"`
}

// checkOnly 校验已保存凭据；替换必须先远端验证成功，再原子更新密文和认证暂停状态。
func (m *Monitoring) SaveAccount(ctx context.Context, input AccountInput, checkOnly bool) (account repository.Account, err error) {
	ctx, span := logging.Start(ctx, "service.save_nga_account")
	defer span.End(&err)
	if !m.enabled {
		return account, logging.WithStack(ErrBackgroundDisabled)
	}
	if err = m.lock(); err != nil {
		return account, err
	}
	defer m.work.Unlock()
	account, err = m.store.Account(ctx)
	if err != nil {
		return account, err
	}
	var credentials infrastructure.Credentials
	if checkOnly {
		if len(account.Cookie) == 0 {
			return account, InvalidInput("请先保存 NGA Cookie")
		}
		credentials, err = m.cipher.Decrypt(ctx, account.Cookie)
	} else {
		credentials, err = infrastructure.ParseCredentials(input.Cookie, input.PassportUID, input.PassportCID)
	}
	if err != nil {
		return account, err
	}
	zerolog.Ctx(ctx).Info().Bool("check_only", checkOnly).Bool("full_cookie", credentials.FullCookie).Msg("Validating NGA credentials")
	checkCtx, cancel := context.WithTimeout(ctx, 25*time.Second)
	stop := context.AfterFunc(m.ctx, cancel)
	defer stop()
	defer cancel()
	checkErr := m.nga.CheckCredentials(checkCtx, credentials)
	now := time.Now().UTC()
	account.CheckedAt = &now
	if checkErr != nil {
		account.LastError = FailureMessage(checkErr)
		if !checkOnly {
			account.LastError = "新凭据未保存：" + account.LastError
		}
		triggerRenewal := checkOnly && account.Status != "auth_paused" && errors.Is(checkErr, infrastructure.ErrNGAAuth)
		if checkOnly && errors.Is(checkErr, infrastructure.ErrNGAAuth) {
			account.Status = "auth_paused"
		}
		// HTTP 取消时也保留本次校验结果；不覆盖旧 Cookie 或旧可用状态。
		writeCtx, done := context.WithTimeout(context.WithoutCancel(ctx), 5*time.Second)
		defer done()
		err = m.store.Transaction(writeCtx, func(ctx context.Context, tx *repository.Store) error {
			if e := tx.SaveAccount(ctx, &account); e != nil {
				return e
			}
			if account.Status == "auth_paused" {
				if e := tx.EnsureAuthAlert(ctx); e != nil {
					return e
				}
				return tx.SetAuthPaused(ctx, true)
			}
			return nil
		})
		if err == nil && triggerRenewal {
			m.authRenewal(writeCtx)
		}
		return account, errors.Join(checkErr, err)
	}
	if !checkOnly {
		account.Cookie, err = m.cipher.Encrypt(ctx, credentials.Cookie)
		if err != nil {
			return account, err
		}
		uid := credentials.UID
		if len(uid) > 3 {
			uid = uid[len(uid)-3:]
		}
		account.UIDMasked, account.FullCookie = "***"+uid, credentials.FullCookie
	}
	account.Status, account.LastError = "valid", ""
	err = m.store.Transaction(ctx, func(ctx context.Context, tx *repository.Store) error {
		if e := tx.SaveAccount(ctx, &account); e != nil {
			return e
		}
		if e := tx.ResolveAuthAlert(ctx); e != nil {
			return e
		}
		return tx.SetAuthPaused(ctx, false)
	})
	return account, err
}

type WatchInput struct {
	ChannelIDs         []int64                   `json:"channel_ids" form:"channel_ids"`
	AuthorUIDs         []int64                   `json:"author_uids" form:"-"`
	Kind               string                    `json:"kind" form:"kind"`
	TID                int64                     `json:"tid" form:"tid"`
	UID                int64                     `json:"uid" form:"uid"`
	Label              string                    `json:"label" form:"label"`
	InitMode           string                    `json:"init_mode" form:"init_mode"`
	HistoryConcurrency *int                      `json:"history_concurrency" form:"history_concurrency"`
	IntervalSeconds    *int                      `json:"interval_seconds" form:"interval_seconds"`
	IntervalRules      []repository.IntervalRule `json:"interval_rules" form:"-"`
	NoFetchPeriods     []repository.TimeWindow   `json:"no_fetch_periods" form:"-"`
}

func (m *Monitoring) SaveWatch(ctx context.Context, id int64, input WatchInput) (watch repository.Watch, err error) {
	ctx, span := logging.Start(ctx, "service.save_watch")
	defer span.End(&err)
	if input.Kind == "" {
		input.Kind = "tid"
	}
	if input.Kind == "uid" {
		input.InitMode = "from_now"
	}
	if input.Kind != "tid" && input.Kind != "uid" || input.Kind == "tid" && (input.TID <= 0 || input.UID != 0) || input.Kind == "uid" && (input.UID <= 0 || input.TID != 0) {
		return watch, InvalidInput("请选择 TID 或 UID 监控，只填写对应的正整数 ID")
	}
	if input.InitMode != "full" && input.InitMode != "from_now" {
		return watch, InvalidInput("初始化模式为 full 或 from_now")
	}
	if err = m.lockWatch(id); err != nil {
		return watch, err
	}
	defer m.unlockWatch(id)
	if id != 0 {
		watch, err = m.store.Watch(ctx, id)
		if err != nil {
			return watch, err
		}
	}
	if input.ChannelIDs != nil {
		watch.ChannelIDs = input.ChannelIDs
	}
	if input.AuthorUIDs != nil {
		watch.AuthorUIDs = input.AuthorUIDs
	}
	concurrency := watch.HistoryConcurrency
	if id == 0 || input.Kind == "uid" {
		concurrency = 1
	}
	if input.HistoryConcurrency != nil {
		concurrency = *input.HistoryConcurrency
	}
	if concurrency < 1 || concurrency > 16 || input.Kind == "uid" && concurrency != 1 {
		return watch, InvalidInput("历史页面并发数必须为 1～16，仅 TID 全量初始化支持大于 1")
	}
	watch.HistoryConcurrency = concurrency
	interval := watch.IntervalSeconds
	if id == 0 {
		interval = 60
	}
	if input.IntervalSeconds != nil {
		interval = *input.IntervalSeconds
	}
	// 旧 API 省略调度字段时保留配置；显式空数组表示清空规则。
	if input.IntervalRules == nil {
		input.IntervalRules = watch.IntervalRules
	}
	if input.NoFetchPeriods == nil {
		input.NoFetchPeriods = watch.NoFetchPeriods
	}
	if err = validateSchedule(interval, input.IntervalRules, input.NoFetchPeriods); err != nil {
		return watch, err
	}
	watches, err := m.store.Watches(ctx)
	if err != nil {
		return watch, err
	}
	for _, existing := range watches {
		if existing.ID != id && existing.Kind == input.Kind && (input.Kind == "tid" && existing.TID == input.TID || input.Kind == "uid" && existing.UID == input.UID) {
			return watch, InvalidInput("此目标已有监控")
		}
	}
	resetGaps := watch.TID != input.TID || watch.UID != input.UID || watch.InitMode != input.InitMode
	if resetGaps {
		resetBaseline(&watch)
	}
	if watch.TID != input.TID || watch.UID != input.UID {
		watch.Title = ""
	}
	watch.Kind, watch.TID, watch.UID = input.Kind, input.TID, input.UID
	watch.Label, watch.InitMode = strings.TrimSpace(input.Label), input.InitMode
	watch.IntervalSeconds, watch.IntervalRules, watch.NoFetchPeriods = interval, input.IntervalRules, input.NoFetchPeriods
	now := time.Now().UTC()
	watch.NextRunAt = &now
	zerolog.Ctx(ctx).Info().Int64("watch_id", id).Int64("tid", input.TID).Int64("uid", input.UID).
		Str("kind", input.Kind).Str("init_mode", input.InitMode).Int("history_concurrency", concurrency).Int("interval_seconds", interval).
		Int("interval_rules", len(input.IntervalRules)).Int("no_fetch_periods", len(input.NoFetchPeriods)).Msg("Saving watch")
	err = m.store.Transaction(ctx, func(ctx context.Context, tx *repository.Store) error {
		if resetGaps && id > 0 {
			if e := tx.ClearFloorGaps(ctx, id); e != nil {
				return e
			}
		}
		if e := validateNotificationWatch(ctx, tx, watch); e != nil {
			return e
		}
		account, e := tx.Account(ctx)
		if e != nil {
			return e
		}
		if account.Status == "auth_paused" && watch.State == "ready" {
			watch.State = "auth_paused"
		}
		return tx.SaveWatch(ctx, &watch)
	})
	m.describeWatch(&watch, now)
	return watch, err
}

func resetBaseline(watch *repository.Watch) {
	watch.TopicCursor, watch.ReplyCursor = repository.UserCursor{}, repository.UserCursor{}
	watch.RemoteRows, watch.RemoteTotalPages = 0, 0
	watch.BaselineComplete, watch.CursorFloor, watch.HistoryFloor, watch.HistoryBefore, watch.State = false, 0, -1, nil, "ready"
}

func (m *Monitoring) ChangeWatch(ctx context.Context, id int64, action, mode string) (watch repository.Watch, err error) {
	ctx, span := logging.Start(ctx, "service.change_watch")
	defer span.End(&err)
	if err = m.lockWatch(id); err != nil {
		return watch, err
	}
	defer m.unlockWatch(id)
	watch, err = m.store.Watch(ctx, id)
	if err != nil {
		return watch, err
	}
	zerolog.Ctx(ctx).Info().Int64("watch_id", id).Str("action", action).Msg("Changing watch state")
	switch action {
	case "pause":
		watch.Paused = true
	case "resume":
		watch.Paused = false
		now := time.Now().UTC()
		watch.NextRunAt = &now
	case "reset":
		if watch.Kind == "uid" {
			mode = "from_now"
		}
		if mode != "full" && mode != "from_now" {
			return watch, InvalidInput("请选择重建基线的初始化模式")
		}
		state := watch.State
		resetBaseline(&watch)
		if state == "auth_paused" {
			watch.State = state
		}
		watch.InitMode = mode
		now := time.Now().UTC()
		watch.NextRunAt = &now
	case "delete":
		return watch, m.store.Transaction(ctx, func(ctx context.Context, tx *repository.Store) error {
			if e := tx.ClearFloorGaps(ctx, id); e != nil {
				return e
			}
			return tx.DeleteWatch(ctx, id)
		})
	default:
		return watch, InvalidInput("不支持的监控操作")
	}
	err = m.store.Transaction(ctx, func(ctx context.Context, tx *repository.Store) error {
		if action == "reset" {
			if e := tx.ClearFloorGaps(ctx, id); e != nil {
				return e
			}
		}
		account, e := tx.Account(ctx)
		if e != nil {
			return e
		}
		if account.Status == "auth_paused" && watch.State == "ready" {
			watch.State = "auth_paused"
		}
		return tx.SaveWatch(ctx, &watch)
	})
	m.describeWatch(&watch, time.Now())
	return watch, err
}

type WatchDetail struct {
	Backfills         []repository.Backfill `json:"backfills"`
	Gaps              repository.GapSummary `json:"gaps"`
	Channels          []repository.Channel  `json:"channels"`
	Timezone          string                `json:"timezone"`
	BackgroundEnabled bool                  `json:"background_enabled"`
	Watch             repository.Watch      `json:"watch"`
	Runs              []repository.Run      `json:"runs"`
}

func (m *Monitoring) Watch(ctx context.Context, id int64) (detail WatchDetail, err error) {
	ctx, span := logging.Start(ctx, "service.watch_detail")
	defer span.End(&err)
	detail.Timezone, detail.BackgroundEnabled = m.location.String(), m.enabled
	detail.Watch, err = m.store.Watch(ctx, id)
	if err != nil {
		return detail, err
	}
	m.describeWatch(&detail.Watch, time.Now())
	detail.Channels, err = m.store.Channels(ctx)
	if err != nil {
		return detail, err
	}
	if detail.Watch.Kind == "uid" {
		detail.Backfills, err = m.store.Backfills(ctx, id)
		if err != nil {
			return detail, err
		}
	} else {
		gaps, e := m.store.FloorGaps(ctx, id)
		if e != nil {
			return detail, e
		}
		for _, gap := range gaps {
			switch gap.Status {
			case "pending":
				detail.Gaps.Pending++
			case "resolved":
				detail.Gaps.Resolved++
			case "expired":
				detail.Gaps.Expired++
			}
		}
	}
	detail.Runs, err = m.store.WatchRuns(ctx, id)
	return detail, err
}

func (m *Monitoring) Run(ctx context.Context, id int64) (run repository.Run, err error) {
	ctx, span := logging.Start(ctx, "service.run_status")
	defer span.End(&err)
	return m.store.Run(ctx, id)
}

// 对页面和持久化摘要只提供已知安全的错误语义；内部原因和栈由结束边界统一记录。
func FailureMessage(err error) string {
	if errors.Is(err, infrastructure.ErrNGAUserMissing) {
		return "用户不存在，已停止自动采集"
	}
	var login *infrastructure.LoginError
	if errors.As(err, &login) {
		labels := map[string]string{"protocol_changed": "NGA 登录协议已变化，请手动更新 Cookie", "invalid_captcha_image": "验证码图片无效，请重新发起", "invalid_captcha": "验证码错误，请重新发起", "invalid_credentials": "NGA 登录名或密码错误，请修改配置后重试", "unsupported_challenge": "NGA 要求手机、短信或腾讯验证，请手动更新 Cookie", "busy": "NGA 服务器忙，请稍后重新发起", "candidate_cookie_missing": "登录响应未提供可校验 Cookie，请手动更新", "login_rejected": "NGA 拒绝登录，请检查账号后重新发起", "response_too_large": "NGA 登录响应异常，请手动更新 Cookie"}
		if text, ok := labels[login.Code]; ok {
			return text
		}
	}
	var input *InputError
	if errors.As(err, &input) {
		return input.Message
	}
	for _, known := range []error{ErrBusy, ErrPaused, ErrBackgroundDisabled, infrastructure.ErrCredentials, infrastructure.ErrNGAAuth,
		infrastructure.ErrNGAMissing, infrastructure.ErrNGAPending, infrastructure.ErrNGABusy, infrastructure.ErrNGASearchUnavailable} {
		if errors.Is(err, known) {
			return known.Error()
		}
	}
	if errors.Is(err, repository.ErrNotFound) {
		return "记录不存在"
	}
	if errors.Is(err, context.Canceled) {
		return "操作已中断，请手动重试"
	}
	if errors.Is(err, context.DeadlineExceeded) {
		return "请求超时，请稍后重试"
	}
	return "操作失败，请凭关联 ID 查看日志"
}

func (m *Monitoring) TaskContext() (context.Context, context.CancelFunc) {
	ctx, cancel := context.WithCancel(m.log.WithContext(context.Background()))
	stop := context.AfterFunc(m.ctx, cancel)
	return ctx, func() { stop(); cancel() }
}
