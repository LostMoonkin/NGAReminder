package service

import (
	"context"
	"errors"
	"time"

	"github.com/rs/zerolog"

	"ngareminder/service/internal/infrastructure"
	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/repository"
)

func (m *Monitoring) StartRun(ctx context.Context, id int64) (run repository.Run, err error) {
	ctx, span := logging.Start(ctx, "service.start_run")
	defer span.End(&err)
	if !m.enabled {
		return run, logging.WithStack(ErrBackgroundDisabled)
	}
	if err = m.lock(); err != nil {
		return run, err
	}
	started := false
	defer func() {
		if !started {
			m.work.Unlock()
		}
	}()
	watch, err := m.store.Watch(ctx, id)
	if err != nil {
		return run, err
	}
	account, err := m.store.Account(ctx)
	if err != nil {
		return run, err
	}
	if len(account.Cookie) == 0 {
		return run, InvalidInput("请先保存并验证 NGA Cookie")
	}
	if account.Status != "valid" {
		return run, logging.WithStack(infrastructure.ErrNGAAuth)
	}
	credentials, err := m.cipher.Decrypt(ctx, account.Cookie)
	if err != nil {
		return run, err
	}
	// HTTP 返回后仍需运行，用进程 context 建立独立 trace，并记录来源请求。
	taskCtx, taskSpan := logging.Start(m.log.WithContext(context.Background()), "service.collect_thread")
	taskCtx, cancel := context.WithCancel(taskCtx)
	stop := context.AfterFunc(m.ctx, cancel)
	run = repository.Run{WatchID: watch.ID, TID: watch.TID, Source: "manual", Status: "running", Silent: !watch.BaselineComplete,
		StartedAt: time.Now().UTC(), TraceID: taskSpan.TraceID, SourceTraceID: logging.TraceID(ctx)}
	if err = m.store.SaveRun(ctx, &run); err != nil {
		stop()
		cancel()
		taskSpan.End(&err)
		return run, err
	}
	zerolog.Ctx(ctx).Info().Int64("run_id", run.ID).Int64("watch_id", id).Str("run_trace_id", run.TraceID).Msg("已开始手动采集")
	started = true
	go func(run repository.Run) {
		defer m.work.Unlock()
		defer cancel()
		defer stop()
		var runErr error
		defer taskSpan.End(&runErr)
		defer func() {
			if value := recover(); value != nil {
				runErr = logging.FromPanic(value)
			}
			if runErr != nil {
				finishCtx, done := context.WithTimeout(context.WithoutCancel(taskCtx), 5*time.Second)
				defer done()
				runErr = errors.Join(runErr, m.finishFailedRun(finishCtx, &run, &watch, runErr))
				logging.Error(taskCtx, runErr, "主题采集未完成", zerolog.ErrorLevel)
			}
			zerolog.Ctx(taskCtx).Info().Int64("run_id", run.ID).Str("status", run.Status).Int("pages", run.Pages).Int64("saved", run.Saved).Msg("主题采集结束")
		}()
		zerolog.Ctx(taskCtx).Info().Int64("run_id", run.ID).Int64("watch_id", watch.ID).Int64("tid", watch.TID).
			Str("source_trace_id", run.SourceTraceID).Str("init_mode", watch.InitMode).Bool("silent", run.Silent).Msg("开始采集主题")
		runErr = m.collect(taskCtx, credentials, &watch, &run)
	}(run)
	return run, nil
}

func (m *Monitoring) collect(ctx context.Context, credentials infrastructure.Credentials, watch *repository.Watch, run *repository.Run) error {
	first, err := m.nga.ThreadPage(ctx, credentials, watch.TID, 1)
	if err != nil {
		return err
	}
	watch.Title = first.Title
	maxFloor := watch.CursorFloor
	if !watch.BaselineComplete && watch.InitMode == "from_now" {
		last := first
		run.Pages = 1
		if first.TotalPages > 1 {
			last, err = m.nga.ThreadPage(ctx, credentials, watch.TID, first.TotalPages)
			if err != nil {
				return err
			}
			run.Pages++
		}
		for _, post := range last.Posts {
			if post.Kind != "comment" && post.Floor > maxFloor {
				maxFloor = post.Floor
			}
		}
		now := time.Now().UTC()
		watch.HistoryFloor, watch.HistoryBefore = maxFloor, &now
		return m.finishSuccessfulRun(ctx, watch, run, maxFloor)
	}
	// 当前手动采集逐页读、按自然键增量写，也能发现旧楼层下新增的楼中楼。
	// 页数固定为首个响应的快照；采集期间继续增长的尾页留到下一轮，避免追赶不停。
	for page := 1; page <= first.TotalPages; page++ {
		result := first
		if page > 1 {
			result, err = m.nga.ThreadPage(ctx, credentials, watch.TID, page)
			if err != nil {
				return err
			}
		}
		posts := []repository.Post{}
		for _, post := range result.Posts {
			if post.Kind != "comment" && post.Floor > maxFloor {
				maxFloor = post.Floor
			}
			if watch.HistoryBefore != nil {
				if post.Kind != "comment" && post.Floor <= watch.HistoryFloor {
					continue
				}
				if post.Kind == "comment" && post.ParentFloor <= watch.HistoryFloor && (post.PublishedAt == nil || !post.PublishedAt.After(*watch.HistoryBefore)) {
					continue
				}
			}
			posts = append(posts, repository.Post{TID: post.TID, Key: post.Key, PID: post.PID, Kind: post.Kind, Floor: post.Floor,
				ParentKey: post.ParentKey, ParentFloor: post.ParentFloor, CommentToID: post.CommentToID, AuthorUID: post.AuthorUID,
				Author: post.Author, Subject: post.Subject, Body: post.Body, PublishedAt: post.PublishedAt, SourceURL: post.SourceURL, Resources: post.Resources})
		}
		next := *run
		err = m.store.Transaction(ctx, func(ctx context.Context, tx *repository.Store) error {
			saved, e := tx.InsertPosts(ctx, posts)
			if e != nil {
				return e
			}
			next.Pages, next.Saved = next.Pages+1, next.Saved+saved
			return tx.SaveRun(ctx, &next)
		})
		if err != nil {
			return err
		}
		*run = next
		zerolog.Ctx(ctx).Info().Int64("watch_id", watch.ID).Int("page", page).Int("total_pages", first.TotalPages).
			Int64("saved", run.Saved).Msg("主题页已保存，完成全轮后推进水位")
	}
	return m.finishSuccessfulRun(ctx, watch, run, maxFloor)
}

func (m *Monitoring) finishSuccessfulRun(ctx context.Context, watch *repository.Watch, run *repository.Run, maxFloor int64) error {
	now := time.Now().UTC()
	watch.BaselineComplete, watch.CursorFloor, watch.State = true, maxFloor, "ready"
	run.Status, run.FinishedAt = "success", &now
	return m.store.Transaction(ctx, func(ctx context.Context, tx *repository.Store) error {
		if err := tx.SaveWatch(ctx, watch); err != nil {
			return err
		}
		return tx.SaveRun(ctx, run)
	})
}

func (m *Monitoring) finishFailedRun(ctx context.Context, run *repository.Run, watch *repository.Watch, cause error) error {
	now := time.Now().UTC()
	run.Status, run.FinishedAt, run.Error = "failed", &now, FailureMessage(cause)
	switch {
	case errors.Is(cause, context.Canceled):
		run.Status = "interrupted"
	case errors.Is(cause, infrastructure.ErrNGAPending):
		run.Status = "skipped_pending"
	case errors.Is(cause, infrastructure.ErrNGABusy):
		run.Status = "skipped_busy"
	case errors.Is(cause, infrastructure.ErrNGAAuth):
		run.Status = "auth_paused"
	case errors.Is(cause, infrastructure.ErrNGAMissing):
		run.Status = "missing"
	}
	return m.store.Transaction(ctx, func(ctx context.Context, tx *repository.Store) error {
		if run.Status == "auth_paused" {
			account, err := tx.Account(ctx)
			if err != nil {
				return err
			}
			account.Status, account.LastError, account.CheckedAt = "auth_paused", run.Error, &now
			if err = tx.SaveAccount(ctx, &account); err != nil {
				return err
			}
			if err = tx.SetAuthPaused(ctx, true); err != nil {
				return err
			}
		}
		if run.Status == "missing" {
			// 重新读取持久化水位，不能写入本轮尚未提交的内存进度。
			persisted, err := tx.Watch(ctx, watch.ID)
			if err != nil {
				return err
			}
			persisted.State = "missing"
			if err = tx.SaveWatch(ctx, &persisted); err != nil {
				return err
			}
		}
		return tx.SaveRun(ctx, run)
	})
}
