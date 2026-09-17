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
	ErrBusy               = errors.New("已有采集或账号操作正在执行，请完成后再试")
	ErrBackgroundDisabled = errors.New("后台任务已关闭，核验模式不访问 NGA")
)

type InputError struct{ Message string }

func (e *InputError) Error() string     { return e.Message }
func InvalidInput(message string) error { return logging.WithStack(&InputError{message}) }

type Monitoring struct {
	store   *repository.Store
	nga     *infrastructure.NGA
	cipher  *infrastructure.CredentialCipher
	log     *logging.Logger
	enabled bool
	ctx     context.Context
	cancel  context.CancelFunc
	// 一个进程只执行一次采集/凭据变更。运行时仍可浏览数据，不维护任务队列或租约。
	work sync.Mutex
}

func NewMonitoring(ctx context.Context, cfg config.Config, store *repository.Store, nga *infrastructure.NGA, log *logging.Logger) (monitor *Monitoring, err error) {
	initCtx, span := logging.Start(ctx, "service.initialize_monitoring")
	defer span.End(&err)
	cipher, err := infrastructure.NewCredentialCipher(cfg.EncryptionKey)
	if err != nil {
		return nil, err
	}
	if err = store.InterruptRuns(initCtx); err != nil {
		return nil, err
	}
	lifeCtx, cancel := context.WithCancel(ctx)
	return &Monitoring{store: store, nga: nga, cipher: cipher, log: log, enabled: cfg.BackgroundEnabled, ctx: lifeCtx, cancel: cancel}, nil
}

func (m *Monitoring) Close() {
	_, span := logging.Start(m.log.WithContext(m.ctx), "service.stop_monitoring")
	defer span.End(nil)
	m.cancel()
	m.work.Lock()
	defer m.work.Unlock()
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
				return tx.SetAuthPaused(ctx, true)
			}
			return nil
		})
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
		return tx.SetAuthPaused(ctx, false)
	})
	return account, err
}

type WatchInput struct {
	TID      int64  `json:"tid" form:"tid"`
	Label    string `json:"label" form:"label"`
	InitMode string `json:"init_mode" form:"init_mode"`
}

func (m *Monitoring) SaveWatch(ctx context.Context, id int64, input WatchInput) (watch repository.Watch, err error) {
	ctx, span := logging.Start(ctx, "service.save_watch")
	defer span.End(&err)
	if input.TID <= 0 || (input.InitMode != "full" && input.InitMode != "from_now") {
		return watch, InvalidInput("TID 必须为正整数，初始化模式为 full 或 from_now")
	}
	if err = m.lock(); err != nil {
		return watch, err
	}
	defer m.work.Unlock()
	if id != 0 {
		watch, err = m.store.Watch(ctx, id)
		if err != nil {
			return watch, err
		}
	}
	watches, err := m.store.Watches(ctx)
	if err != nil {
		return watch, err
	}
	for _, existing := range watches {
		if existing.TID == input.TID && existing.ID != id {
			return watch, InvalidInput("此 TID 已有监控")
		}
	}
	if watch.TID != input.TID || watch.InitMode != input.InitMode {
		resetBaseline(&watch)
	}
	if watch.TID != input.TID {
		watch.Title = ""
	}
	watch.TID, watch.Label, watch.InitMode = input.TID, strings.TrimSpace(input.Label), input.InitMode
	zerolog.Ctx(ctx).Info().Int64("watch_id", id).Int64("tid", input.TID).Str("init_mode", input.InitMode).Msg("Saving TID watch")
	err = m.store.SaveWatch(ctx, &watch)
	return watch, err
}

func resetBaseline(watch *repository.Watch) {
	watch.BaselineComplete, watch.CursorFloor, watch.HistoryFloor, watch.HistoryBefore, watch.State = false, 0, -1, nil, "ready"
}

func (m *Monitoring) ChangeWatch(ctx context.Context, id int64, action, mode string) (watch repository.Watch, err error) {
	ctx, span := logging.Start(ctx, "service.change_watch")
	defer span.End(&err)
	if err = m.lock(); err != nil {
		return watch, err
	}
	defer m.work.Unlock()
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
	case "reset":
		if mode != "full" && mode != "from_now" {
			return watch, InvalidInput("请选择重建基线的初始化模式")
		}
		resetBaseline(&watch)
		watch.InitMode = mode
	case "delete":
		return watch, m.store.DeleteWatch(ctx, id)
	default:
		return watch, InvalidInput("不支持的监控操作")
	}
	err = m.store.SaveWatch(ctx, &watch)
	return watch, err
}

type WatchDetail struct {
	Watch repository.Watch `json:"watch"`
	Runs  []repository.Run `json:"runs"`
}

func (m *Monitoring) Watch(ctx context.Context, id int64) (detail WatchDetail, err error) {
	ctx, span := logging.Start(ctx, "service.watch_detail")
	defer span.End(&err)
	detail.Watch, err = m.store.Watch(ctx, id)
	if err != nil {
		return detail, err
	}
	detail.Runs, err = m.store.Runs(ctx, detail.Watch.TID)
	return detail, err
}

type ThreadContent struct {
	TID   int64             `json:"tid"`
	Posts []repository.Post `json:"posts"`
	Total int64             `json:"total"`
	Page  int               `json:"page"`
	Runs  []repository.Run  `json:"runs"`
}

func (m *Monitoring) Posts(ctx context.Context, tid int64, page int) (data ThreadContent, err error) {
	ctx, span := logging.Start(ctx, "service.thread_content")
	defer span.End(&err)
	data.TID, data.Page = tid, page
	data.Posts, data.Total, err = m.store.Posts(ctx, tid, page)
	if err != nil {
		return data, err
	}
	data.Runs, err = m.store.Runs(ctx, tid)
	return data, err
}

func (m *Monitoring) Run(ctx context.Context, id int64) (run repository.Run, err error) {
	ctx, span := logging.Start(ctx, "service.run_status")
	defer span.End(&err)
	return m.store.Run(ctx, id)
}

// 对页面和持久化摘要只提供已知安全的错误语义；内部原因和栈由结束边界统一记录。
func FailureMessage(err error) string {
	var input *InputError
	if errors.As(err, &input) {
		return input.Message
	}
	for _, known := range []error{ErrBusy, ErrBackgroundDisabled, infrastructure.ErrCredentials, infrastructure.ErrNGAAuth,
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
