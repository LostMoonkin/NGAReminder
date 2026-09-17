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
	run, started, err = m.startRunLocked(ctx, watch, "manual", time.Now())
	return run, err
}

// 调度和手动入口共用状态判断，启动成功后由采集 goroutine 持有串行锁直到收尾。
func (m *Monitoring) startRunLocked(ctx context.Context, watch repository.Watch, source string, now time.Time) (run repository.Run, started bool, err error) {
	if watch.Paused {
		return run, false, logging.WithStack(ErrPaused)
	}
	if watch.State == "auth_paused" {
		return run, false, logging.WithStack(infrastructure.ErrNGAAuth)
	}
	account, err := m.store.Account(ctx)
	if err != nil {
		return run, false, err
	}
	if len(account.Cookie) == 0 {
		return run, false, InvalidInput("请先保存并验证 NGA Cookie")
	}
	if account.Status != "valid" {
		return run, false, logging.WithStack(infrastructure.ErrNGAAuth)
	}
	// HTTP 返回后仍需运行，用进程 context 建立独立 trace，并记录来源请求。
	taskCtx, taskSpan := logging.Start(m.log.WithContext(context.Background()), "service.collect_"+map[string]string{"tid": "thread", "uid": "user"}[watch.Kind])
	taskCtx, cancel := context.WithCancel(taskCtx)
	stop := context.AfterFunc(m.ctx, cancel)
	run = repository.Run{WatchID: watch.ID, TID: watch.TID, UID: watch.UID, Source: source, Status: "running", Silent: !watch.BaselineComplete,
		StartedAt: now.UTC(), TraceID: taskSpan.TraceID, SourceTraceID: logging.TraceID(ctx)}
	if source != "gap_recovery" {
		watch.NextRunAt = m.nextRun(watch, now)
	}
	if active, until := noFetchAt(watch.NoFetchPeriods, now.In(m.location)); source != "manual" && active {
		finished := now.UTC()
		run.Status, run.FinishedAt, run.Error = "skipped_no_fetch", &finished, "当前处于免拉取时段"
		watch.NextRunAt = until
	}
	err = m.store.Transaction(ctx, func(ctx context.Context, tx *repository.Store) error {
		if watch.Kind == "tid" && run.Status == "running" {
			if e := advanceGapAttempts(ctx, tx, watch.ID, now); e != nil {
				return e
			}
		}
		if e := tx.SaveWatch(ctx, &watch); e != nil {
			return e
		}
		return tx.SaveRun(ctx, &run)
	})
	if err != nil || run.Status == "skipped_no_fetch" {
		zerolog.Ctx(taskCtx).Info().Int64("watch_id", watch.ID).Int64("run_id", run.ID).
			Str("source", source).Str("status", run.Status).Str("source_trace_id", run.SourceTraceID).Any("next_run_at", watch.NextRunAt).Msg("Collection not started")
		stop()
		cancel()
		taskSpan.End(&err)
		return run, false, err
	}
	zerolog.Ctx(ctx).Info().Int64("run_id", run.ID).Int64("watch_id", watch.ID).Str("source", source).Str("run_trace_id", run.TraceID).Msg("Collection started")
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
				logging.Error(taskCtx, runErr, "Collection did not complete", zerolog.ErrorLevel)
			}
			zerolog.Ctx(taskCtx).Info().Int64("run_id", run.ID).Str("status", run.Status).Int("pages", run.Pages).Int64("saved", run.Saved).Msg("Collection finished")
		}()
		zerolog.Ctx(taskCtx).Info().Int64("run_id", run.ID).Int64("watch_id", watch.ID).Int64("tid", watch.TID).Int64("uid", watch.UID).Str("source", source).
			Str("source_trace_id", run.SourceTraceID).Str("init_mode", watch.InitMode).Bool("silent", run.Silent).Msg("Starting collection")
		credentials, decryptErr := m.cipher.Decrypt(taskCtx, account.Cookie)
		if decryptErr != nil {
			runErr = decryptErr
			return
		}
		if source == "gap_recovery" {
			runErr = m.collectGaps(taskCtx, credentials, &watch, &run)
		} else if watch.Kind == "uid" {
			runErr = m.collectUser(taskCtx, credentials, &watch, &run)
		} else {
			runErr = m.collect(taskCtx, credentials, &watch, &run)
		}
	}(run)
	return run, true, nil
}

func (m *Monitoring) collect(ctx context.Context, credentials infrastructure.Credentials, watch *repository.Watch, run *repository.Run) error {
	first, err := m.nga.ThreadPage(ctx, credentials, watch.TID, 1)
	if err != nil {
		return err
	}
	watch.Title = first.Title
	gaps, err := loadGapMap(ctx, m.store, watch.ID)
	if err != nil {
		return err
	}
	seen := map[int64]bool{}
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
		return m.finishSuccessfulRun(ctx, watch, run, maxFloor, nil, nil)
	}
	// 采集逐页读、按自然键增量写，也能发现旧楼层下新增的楼中楼。
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
			if post.Kind != "comment" {
				seen[post.Floor] = true
			}
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
			if watch.BaselineComplete && post.Kind != "comment" && post.Floor <= watch.CursorFloor {
				if _, ok := gaps[post.Floor]; !ok {
					continue
				}
			}
			posts = append(posts, storedPost(post))
		}
		next := *run
		err = m.store.Transaction(ctx, func(ctx context.Context, tx *repository.Store) error {
			saved, e := tx.InsertPosts(ctx, posts)
			if e != nil {
				return e
			}
			if e = m.recordThreadPosts(ctx, tx, *watch, posts, run.Silent, gaps); e != nil {
				return e
			}
			next.Pages, next.Saved = next.Pages+1, next.Saved+saved
			return tx.SaveRun(ctx, &next)
		})
		if err != nil {
			return err
		}
		*run = next
		m.resources.Collect(ctx, posts)
		zerolog.Ctx(ctx).Info().Int64("watch_id", watch.ID).Int("page", page).Int("total_pages", first.TotalPages).
			Int64("saved", run.Saved).Msg("Thread page saved; cursor will advance after the entire run completes")
	}
	return m.finishSuccessfulRun(ctx, watch, run, maxFloor, nil, newFloorGaps(*watch, seen, maxFloor, time.Now()))
}

func (m *Monitoring) finishSuccessfulRun(ctx context.Context, watch *repository.Watch, run *repository.Run, maxFloor int64, posts []repository.Post, newGaps []repository.FloorGap) error {
	now := time.Now().UTC()
	watch.NextRunAt = m.nextRun(*watch, now)
	watch.BaselineComplete, watch.CursorFloor, watch.State = true, maxFloor, "ready"
	next := *run
	next.Status, next.FinishedAt = "success", &now
	err := m.store.Transaction(ctx, func(ctx context.Context, tx *repository.Store) error {
		if len(newGaps) > 0 {
			zerolog.Ctx(ctx).Info().Int64("watch_id", watch.ID).Int("gap_count", len(newGaps)).Time("deadline", newGaps[0].Deadline).Msg("Recording newly crossed floor gaps")
		}
		if e := tx.AddFloorGaps(ctx, newGaps); e != nil {
			return e
		}
		if watch.Kind == "tid" {
			if e := expireCompletedGaps(ctx, tx, watch.ID); e != nil {
				return e
			}
		}
		saved, err := tx.InsertPosts(ctx, posts)
		if err != nil {
			return err
		}
		if err = m.notifications.Record(ctx, tx, *watch, posts, run.Silent); err != nil {
			return err
		}
		next.Saved += saved
		if err := tx.SaveWatch(ctx, watch); err != nil {
			return err
		}
		return tx.SaveRun(ctx, &next)
	})
	if err == nil {
		m.resources.Collect(ctx, posts)
		*run = next
	}
	return err
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
	triggerRenewal := false
	err := m.store.Transaction(ctx, func(ctx context.Context, tx *repository.Store) error {
		if run.Status == "auth_paused" {
			var e error
			triggerRenewal, e = pauseAccount(ctx, tx, now, run.Error)
			if e != nil {
				return e
			}
		}
		if watch.Kind == "tid" {
			if e := expireCompletedGaps(ctx, tx, watch.ID); e != nil {
				return e
			}
		}

		// 只更新调度和故障状态，不提交本轮尚未完成的水位。
		persisted, err := tx.Watch(ctx, watch.ID)
		if err != nil {
			return err
		}
		if run.Source != "gap_recovery" {
			persisted.NextRunAt = m.nextRun(persisted, now)
		}
		if run.Status == "missing" {
			persisted.State = "missing"
		}
		if err = tx.SaveWatch(ctx, &persisted); err != nil {
			return err
		}
		return tx.SaveRun(ctx, run)
	})
	if err == nil && triggerRenewal {
		m.authRenewal(ctx)
	}
	return err
}
