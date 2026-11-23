package service

import (
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"sync"
	"time"

	"github.com/rs/zerolog"

	"ngareminder/service/internal/infrastructure"
	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/repository"
)

var (
	ErrExportNotReady = errors.New("导出任务尚未完成或未生成可下载文件")
	ErrExportExpired  = errors.New("导出文件已过期并清理，请重新发起导出")
)

type Exports struct {
	monitor   *Monitoring
	files     *infrastructure.Exports
	threshold int64
	retention time.Duration
	notify    chan struct{}
	startOnce sync.Once
	wg        sync.WaitGroup
	mu        sync.Mutex
}

func (m *Monitoring) Exports() *Exports { return m.exports }
func (e *Exports) Threshold() int64     { return e.threshold }
func (e *Exports) RetentionDays() int   { return int(e.retention / (24 * time.Hour)) }

func (e *Exports) Wake() {
	e.startOnce.Do(func() {
		e.wg.Add(1)
		go e.runWorker()
	})
	select {
	case e.notify <- struct{}{}:
	default:
	}
}

func (e *Exports) Close() {
	e.wg.Wait()
}

func (e *Exports) List(ctx context.Context) (tasks []repository.ExportTask, err error) {
	ctx, span := logging.Start(ctx, "service.list_export_tasks")
	defer span.End(&err)
	if _, err = e.CleanupExpired(ctx, time.Now().UTC()); err != nil {
		return nil, err
	}
	return e.monitor.store.ExportTasks(ctx, 100)
}

func (e *Exports) Task(ctx context.Context, id int64) (task repository.ExportTask, err error) {
	ctx, span := logging.Start(ctx, "service.get_export_task")
	defer span.End(&err)
	e.mu.Lock()
	defer e.mu.Unlock()
	task, err = e.monitor.store.ExportTask(ctx, id)
	if err != nil {
		return task, err
	}
	if e.isExpired(task, time.Now().UTC()) {
		if err = e.expireLocked(ctx, task); err != nil {
			return task, err
		}
		task.Status = "expired"
		task.FileName = ""
	}
	return task, nil
}

func (e *Exports) OpenDownload(ctx context.Context, id int64) (task repository.ExportTask, file *os.File, info os.FileInfo, err error) {
	ctx, span := logging.Start(ctx, "service.open_export_download")
	defer span.End(&err)
	e.mu.Lock()
	defer e.mu.Unlock()
	task, err = e.monitor.store.ExportTask(ctx, id)
	if err != nil {
		return task, nil, nil, err
	}
	if e.isExpired(task, time.Now().UTC()) {
		if err = e.expireLocked(ctx, task); err != nil {
			return task, nil, nil, err
		}
		return task, nil, nil, logging.WithStack(ErrExportExpired)
	}
	switch task.Status {
	case "expired":
		return task, nil, nil, logging.WithStack(ErrExportExpired)
	case "completed":
		if task.FileName == "" {
			return task, nil, nil, logging.WithStack(ErrExportExpired)
		}
	default:
		return task, nil, nil, logging.WithStack(ErrExportNotReady)
	}
	file, info, err = e.files.Open(ctx, task.FileName)
	if errors.Is(err, fs.ErrNotExist) {
		_ = e.monitor.store.MarkExportTaskExpired(ctx, task.ID)
		return task, nil, nil, logging.WithStack(ErrExportExpired)
	}
	if err != nil {
		return task, nil, nil, err
	}
	return task, file, info, nil
}

func (e *Exports) isExpired(task repository.ExportTask, now time.Time) bool {
	return task.Status == "completed" && task.ExpiresAt != nil && !now.UTC().Before(*task.ExpiresAt)
}

func (e *Exports) expireLocked(ctx context.Context, task repository.ExportTask) error {
	if task.FileName != "" {
		if err := e.files.Remove(ctx, task.FileName); err != nil {
			return err
		}
	}
	return e.monitor.store.MarkExportTaskExpired(ctx, task.ID)
}

// CleanupExpired 清理已超过保留期的导出文件；没有过期项时保持静默。
func (e *Exports) CleanupExpired(ctx context.Context, now time.Time) (count int, err error) {
	e.mu.Lock()
	defer e.mu.Unlock()
	probe := logging.Quiet(ctx)
	expired, err := e.monitor.store.ExpiredExportTasks(probe, now)
	if err != nil {
		ctx, span := logging.Start(ctx, "service.cleanup_expired_exports")
		logging.Error(ctx, err, "Check expired export tasks failed", zerolog.ErrorLevel)
		span.End(&err)
		return 0, err
	}
	if len(expired) == 0 {
		return 0, nil
	}
	ctx, span := logging.Start(ctx, "service.cleanup_expired_exports")
	defer span.End(&err)
	for _, task := range expired {
		if err = e.expireLocked(ctx, task); err != nil {
			logging.Error(ctx, err, "Cleanup expired export task failed", zerolog.ErrorLevel)
			return count, err
		}
		count++
	}
	zerolog.Ctx(ctx).Info().Int("expired_count", count).Msg("Expired async exports cleaned up")
	return count, nil
}

func (e *Exports) runWorker() {
	defer e.wg.Done()
	for {
		if e.monitor.ctx.Err() != nil {
			return
		}
		probe := logging.Quiet(e.monitor.log.WithContext(e.monitor.ctx))
		task, found, err := e.monitor.store.NextQueuedExportTask(probe)
		if err != nil {
			if e.monitor.ctx.Err() != nil {
				return
			}
			errCtx, span := logging.Start(e.monitor.log.WithContext(e.monitor.ctx), "service.poll_export_queue")
			logging.Error(errCtx, err, "Poll async export queue failed", zerolog.ErrorLevel)
			span.End(&err)
			select {
			case <-e.monitor.ctx.Done():
				return
			case <-time.After(time.Second):
			}
			continue
		}
		if !found {
			select {
			case <-e.monitor.ctx.Done():
				return
			case <-e.notify:
			}
			continue
		}
		e.runTask(task)
	}
}

func (e *Exports) runTask(task repository.ExportTask) {
	taskCtx, cancel := e.monitor.TaskContext()
	defer cancel()
	taskCtx, span := logging.Start(taskCtx, "service.run_export_task")
	var err error
	defer func() {
		if value := recover(); value != nil {
			err = logging.FromPanic(value)
			e.finishFailed(taskCtx, &task, err)
		}
		span.End(&err)
	}()

	started := time.Now().UTC()
	task.Status = "running"
	task.StartedAt = &started
	task.TraceID = span.TraceID
	if err = e.monitor.store.SaveExportTask(taskCtx, &task); err != nil {
		logging.Error(taskCtx, err, "Mark export task running failed", zerolog.ErrorLevel)
		return
	}
	zerolog.Ctx(taskCtx).Info().Int64("task_id", task.ID).Str("source_trace_id", task.SourceTraceID).
		Str("kind", task.Kind).Int64("target_id", task.TargetID).Str("format", task.Format).
		Int64("posts", task.PostCount).Int("author_uid_count", len(task.AuthorUIDs)).Msg("Running async export task")

	ext, err := exportExtension(task.Format)
	if err != nil {
		e.finishFailed(taskCtx, &task, err)
		return
	}
	filter, err := contentFilter(task.Kind, task.TargetID)
	if err != nil {
		e.finishFailed(taskCtx, &task, err)
		return
	}
	filter.StartAt, filter.EndAt, filter.AuthorUIDs = task.StartAt, task.EndAt, task.AuthorUIDs

	file, tempName, err := e.files.CreateTemp(taskCtx, ext)
	if err != nil {
		e.finishFailed(taskCtx, &task, err)
		return
	}
	published := false
	defer func() {
		if !published {
			cleanCtx, done := context.WithTimeout(context.WithoutCancel(taskCtx), 5*time.Second)
			defer done()
			_ = e.files.Remove(cleanCtx, tempName)
		}
	}()

	assets, writeErr := e.monitor.writeExport(taskCtx, file, task.Kind, task.TargetID, task.Format, filter, task.PostCount, task.SnapshotMaxID)
	info, statErr := file.Stat()
	closeErr := file.Close()
	if err = errors.Join(writeErr, statErr, closeErr); err != nil {
		e.finishFailed(taskCtx, &task, err)
		return
	}

	finalName := fmt.Sprintf("export-%d-%s%s", task.ID, rand.Text(), ext)
	if err = e.files.Publish(taskCtx, tempName, finalName); err != nil {
		e.finishFailed(taskCtx, &task, err)
		return
	}
	published = true

	finished := time.Now().UTC()
	expires := finished.Add(e.retention)
	task.Status = "completed"
	task.FileName = finalName
	task.FileSize = info.Size()
	task.FinishedAt = &finished
	task.ExpiresAt = &expires
	task.Error = ""
	writeCtx, done := context.WithTimeout(context.WithoutCancel(taskCtx), 5*time.Second)
	defer done()
	if err = e.monitor.store.SaveExportTask(writeCtx, &task); err != nil {
		_ = e.files.Remove(writeCtx, finalName)
		logging.Error(taskCtx, err, "Save completed export task failed", zerolog.ErrorLevel)
		return
	}
	zerolog.Ctx(taskCtx).Info().Int64("task_id", task.ID).Str("kind", task.Kind).Int64("target_id", task.TargetID).
		Str("format", task.Format).Int64("posts", task.PostCount).Int("assets", assets).
		Int64("file_size", task.FileSize).Time("expires_at", expires).Msg("Async export completed")
}

func (e *Exports) finishFailed(ctx context.Context, task *repository.ExportTask, cause error) {
	finished := time.Now().UTC()
	task.FinishedAt = &finished
	if errors.Is(cause, context.Canceled) {
		task.Status = "interrupted"
		task.Error = "服务停止，异步导出已中断，请重新发起"
		logging.Error(ctx, cause, "Async export interrupted", zerolog.WarnLevel)
	} else {
		task.Status = "failed"
		task.Error = FailureMessage(cause)
		logging.Error(ctx, cause, "Async export failed", zerolog.ErrorLevel)
	}
	writeCtx, done := context.WithTimeout(context.WithoutCancel(ctx), 5*time.Second)
	defer done()
	if saveErr := e.monitor.store.SaveExportTask(writeCtx, task); saveErr != nil {
		logging.Error(ctx, saveErr, "Save failed export task status failed", zerolog.ErrorLevel)
	}
}
