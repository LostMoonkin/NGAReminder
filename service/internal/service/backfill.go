package service

import (
	"context"
	"errors"
	"github.com/rs/zerolog"
	"ngareminder/service/internal/infrastructure"
	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/repository"
	"time"
)

func (m *Monitoring) StartBackfill(ctx context.Context, watchID int64, date string) (job repository.Backfill, err error) {
	ctx, span := logging.Start(ctx, "service.start_backfill")
	defer span.End(&err)
	if !m.enabled {
		return job, logging.WithStack(ErrBackgroundDisabled)
	}
	start, err := time.ParseInLocation("2006-01-02", date, m.location)
	if err != nil {
		return job, InvalidInput("起始日期请使用 YYYY-MM-DD")
	}
	end := time.Now().UTC()
	if start.After(end) {
		return job, InvalidInput("起始日期不能晚于今天")
	}
	if !m.backfillWork.TryLock() {
		return job, logging.WithStack(ErrBusy)
	}
	if err = m.lockWatch(watchID); err != nil {
		m.backfillWork.Unlock()
		return job, err
	}
	started := false
	defer func() {
		if !started {
			m.unlockWatch(watchID)
			m.backfillWork.Unlock()
		}
	}()
	watch, err := m.store.Watch(ctx, watchID)
	if err != nil {
		return job, err
	}
	if watch.Kind != "uid" {
		return job, InvalidInput("历史回帖回填仅支持已有 UID 监控")
	}
	account, err := m.store.Account(ctx)
	if err != nil {
		return job, err
	}
	if account.Status != "valid" {
		return job, logging.WithStack(infrastructure.ErrNGAAuth)
	}
	task, cancel := m.TaskContext()
	task, root := logging.Start(task, "service.backfill_user_replies")
	job = repository.Backfill{WatchID: watch.ID, UID: watch.UID, StartAt: start.UTC(), EndAt: end, Status: "running", TraceID: root.TraceID, SourceTraceID: logging.TraceID(ctx)}
	if err = m.store.SaveBackfill(ctx, &job); err != nil {
		cancel()
		root.End(&err)
		return job, err
	}
	started = true
	go func(job repository.Backfill) {
		defer m.backfillWork.Unlock()
		defer m.unlockWatch(watchID)
		defer cancel()
		var runErr error
		defer root.End(&runErr)
		defer func() {
			if value := recover(); value != nil {
				runErr = logging.FromPanic(value)
			}
			finish, done := context.WithTimeout(context.WithoutCancel(task), 5*time.Second)
			defer done()
			now := time.Now().UTC()
			job.FinishedAt = &now
			job.Status = "success"
			if runErr != nil {
				job.Status = "failed"
				job.Error = FailureMessage(runErr)
				if errors.Is(runErr, context.Canceled) {
					job.Status = "interrupted"
				}
			}
			trigger := false
			saveErr := m.store.Transaction(finish, func(ctx context.Context, tx *repository.Store) error {
				if errors.Is(runErr, infrastructure.ErrNGAAuth) {
					var e error
					trigger, e = pauseAccount(ctx, tx, now, FailureMessage(runErr))
					if e != nil {
						return e
					}
				}
				return tx.SaveBackfill(ctx, &job)
			})
			runErr = errors.Join(runErr, saveErr)
			if saveErr == nil && trigger {
				m.authRenewal(finish)
			}
			if runErr != nil {
				logging.Error(task, runErr, "UID backfill did not complete", zerolog.ErrorLevel)
			}
			zerolog.Ctx(task).Info().Int64("backfill_id", job.ID).Str("status", job.Status).Int("pages", job.Pages).Int64("saved", job.Saved).Msg("UID backfill finished")
		}()
		zerolog.Ctx(task).Info().Int64("backfill_id", job.ID).Int64("watch_id", watch.ID).Int64("uid", watch.UID).Time("start_at", job.StartAt).Time("end_at", job.EndAt).Str("source_trace_id", job.SourceTraceID).Msg("Starting silent UID reply backfill")
		credentials, e := m.cipher.Decrypt(task, account.Cookie)
		if e != nil {
			runErr = e
			return
		}
		runErr = m.backfillReplies(task, credentials, watch, &job)
	}(job)
	return job, nil
}
func (m *Monitoring) backfillReplies(ctx context.Context, credentials infrastructure.Credentials, watch repository.Watch, job *repository.Backfill) error {
	var previous int64
	hasTotal := false
	for page := 1; ; page++ {
		result, err := m.nga.UserPage(ctx, credentials, job.UID, true, page)
		if err != nil {
			if page > 1 && !hasTotal && errors.Is(err, infrastructure.ErrNGASearchUnavailable) {
				logging.Error(ctx, err, "Backfill pagination ended after a successful page with no total count", zerolog.InfoLevel)
				return nil
			}
			return err
		}
		hasTotal = hasTotal || result.HasTotal
		job.Pages++
		if err = m.store.SaveBackfill(ctx, job); err != nil {
			return err
		}
		reachedStart := false
		for _, candidate := range result.Candidates {
			if previous != 0 && candidate.Timestamp > previous {
				return logging.WithStack(errors.New("NGA backfill reply list is not ordered by descending publication time"))
			}
			previous = candidate.Timestamp
			if candidate.Timestamp < job.StartAt.Unix() {
				reachedStart = true
				continue
			}
			if candidate.Timestamp > job.EndAt.Unix() {
				continue
			}
			post, e := m.nga.PostByPID(ctx, credentials, candidate.TID, candidate.PID)
			if e != nil {
				return e
			}
			next := *job
			next.Pages++
			next.Candidates++
			posts := []repository.Post{}
			published := time.Unix(candidate.Timestamp, 0).UTC()
			if post.PublishedAt != nil {
				published = *post.PublishedAt
			} else {
				post.PublishedAt = &published
			}
			if post.AuthorUID == job.UID && !published.Before(job.StartAt) && !published.After(job.EndAt) {
				posts = append(posts, m.storedPost(post))
			}
			e = m.store.Transaction(ctx, func(ctx context.Context, tx *repository.Store) error {
				saved, e := tx.InsertPosts(ctx, posts)
				if e != nil {
					return e
				}
				if e = m.notifications.Record(ctx, tx, watch, posts, true); e != nil {
					return e
				}
				next.Saved += saved
				return tx.SaveBackfill(ctx, &next)
			})
			if e != nil {
				return e
			}
			*job = next
			m.resources.Collect(ctx, posts)
		}
		zerolog.Ctx(ctx).Info().Int64("backfill_id", job.ID).Int("page", page).Int("candidates", job.Candidates).Int64("saved", job.Saved).Msg("Backfill page processed")
		if reachedStart || !result.HasMore {
			return nil
		}
	}
}
func (m *Monitoring) Backfill(ctx context.Context, id int64) (v repository.Backfill, err error) {
	ctx, span := logging.Start(ctx, "service.backfill_status")
	defer span.End(&err)
	return m.store.Backfill(ctx, id)
}

// 所有采集入口共用认证失效语义；手动暂停和实时调度配置保持原值。
func pauseAccount(ctx context.Context, tx *repository.Store, now time.Time, message string) (changed bool, err error) {
	account, err := tx.Account(ctx)
	if err != nil {
		return false, err
	}
	changed = account.Status != "auth_paused"
	account.Status, account.LastError, account.CheckedAt = "auth_paused", message, &now
	if err = tx.SaveAccount(ctx, &account); err != nil {
		return false, err
	}
	if err = tx.EnsureAuthAlert(ctx); err != nil {
		return false, err
	}
	return changed, tx.SetAuthPaused(ctx, true)
}
