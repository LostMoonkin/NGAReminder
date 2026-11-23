package repository

import (
	"context"
	"errors"
	"time"

	"gorm.io/gorm"
	"gorm.io/gorm/clause"
	"ngareminder/service/internal/logging"
)

type ContentFilter struct {
	TID, UID       int64
	AuthorUIDs     []int64
	StartAt, EndAt *time.Time
}

func (s *Store) contentQuery(ctx context.Context, f ContentFilter) *gorm.DB {
	q := s.db.WithContext(ctx).Model(&Post{})
	if f.TID > 0 {
		q = q.Where("tid = ?", f.TID)
	}
	if f.UID > 0 {
		q = q.Where("author_uid = ?", f.UID)
	}
	if len(f.AuthorUIDs) > 0 {
		q = q.Where("author_uid IN ?", f.AuthorUIDs)
	}
	if f.StartAt != nil {
		q = q.Where("published_at >= ?", *f.StartAt)
	}
	if f.EndAt != nil {
		q = q.Where("published_at <= ?", *f.EndAt)
	}
	return q
}
func (s *Store) ContentStats(ctx context.Context, f ContentFilter) (total, maxID int64, err error) {
	ctx, span := logging.Start(ctx, "repository.content_stats")
	defer span.End(&err)
	var result struct{ Total, MaxID int64 }
	err = s.contentQuery(ctx, f).Select("COUNT(*) AS total, COALESCE(MAX(id), 0) AS max_id").Scan(&result).Error
	return result.Total, result.MaxID, logging.Wrap(err, "read content count and snapshot")
}
func (s *Store) ContentBatch(ctx context.Context, f ContentFilter, offset, limit int, maxID int64) (items []Post, err error) {
	ctx, span := logging.Start(ctx, "repository.content_batch")
	defer span.End(&err)
	items = []Post{}
	err = s.contentQuery(ctx, f).Where("id <= ?", maxID).Order("tid, CASE WHEN kind = 'comment' THEN parent_floor ELSE floor END, CASE WHEN kind = 'comment' THEN 1 ELSE 0 END, id").Offset(offset).Limit(limit).Find(&items).Error
	return items, logging.Wrap(err, "read ordered content batch")
}

// 只检查本次导出范围内的引用目标，不读取正文或扩大 UID 的内容范围。
func (s *Store) ContentReferenceExists(ctx context.Context, f ContentFilter, maxID, tid, pid int64) (found bool, err error) {
	ctx, span := logging.Start(ctx, "repository.content_reference_exists")
	defer span.End(&err)
	q := s.contentQuery(ctx, f).Where("id <= ? AND tid = ?", maxID, tid)
	if pid == 0 {
		q = q.Where("kind = ?", "main")
	} else {
		q = q.Where("pid = ?", pid)
	}
	var rows []int64
	err = q.Select("id").Limit(1).Scan(&rows).Error
	return len(rows) > 0, logging.Wrap(err, "check exported reference target")
}

type ResourceSettings struct {
	ID              int  `json:"-"`
	DownloadEnabled bool `json:"download_enabled"`
}
type Resource struct {
	OriginalName string `json:"original_name"`
	URL          string `json:"url" gorm:"primaryKey"`
	Path         string `json:"path"`
	MIME         string `json:"mime"`
	Size         int64  `json:"size"`
	HTTPStatus   int    `json:"http_status"`
	Ignored      bool   `json:"ignored"`
	Error        string `json:"error"`
}

func (s *Store) ResourceSettings(ctx context.Context) (v ResourceSettings, err error) {
	ctx, span := logging.Start(ctx, "repository.resource_settings")
	defer span.End(&err)
	err = s.db.WithContext(ctx).First(&v, 1).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return ResourceSettings{ID: 1}, nil
	}
	return v, logging.Wrap(err, "read resource settings")
}
func (s *Store) SaveResourceSettings(ctx context.Context, v *ResourceSettings) (err error) {
	ctx, span := logging.Start(ctx, "repository.save_resource_settings")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Save(v).Error, "save resource settings")
}
func (s *Store) Resource(ctx context.Context, source string) (v Resource, err error) {
	ctx, span := logging.Start(ctx, "repository.resource")
	defer span.End(&err)
	err = s.db.WithContext(ctx).Where("url = ?", source).First(&v).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return Resource{URL: source}, nil
	}
	return v, logging.Wrap(err, "read resource")
}
func (s *Store) Resources(ctx context.Context) (items []Resource, err error) {
	ctx, span := logging.Start(ctx, "repository.resources")
	defer span.End(&err)
	items = []Resource{}
	err = s.db.WithContext(ctx).Order("url").Find(&items).Error
	return items, logging.Wrap(err, "read resource metadata")
}
func (s *Store) SaveResource(ctx context.Context, v *Resource) (err error) {
	ctx, span := logging.Start(ctx, "repository.save_resource")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Save(v).Error, "save resource metadata")
}
func (s *Store) IgnoreResources(ctx context.Context, urls []string) (err error) {
	ctx, span := logging.Start(ctx, "repository.ignore_resources")
	defer span.End(&err)
	items := make([]Resource, len(urls))
	for i, url := range urls {
		items[i] = Resource{URL: url, Ignored: true}
	}
	err = s.db.WithContext(ctx).Clauses(clause.OnConflict{
		Columns:   []clause.Column{{Name: "url"}},
		DoUpdates: clause.Assignments(map[string]any{"ignored": true}),
	}).CreateInBatches(&items, 200).Error
	return logging.Wrap(err, "ignore resource metadata")
}
func (s *Store) ResourceReferences(ctx context.Context) (urls []string, err error) {
	ctx, span := logging.Start(ctx, "repository.resource_references")
	defer span.End(&err)
	err = s.db.WithContext(ctx).Raw("SELECT DISTINCT j.value FROM posts p, json_each(p.resources) j WHERE j.type = 'text' ORDER BY j.value").Scan(&urls).Error
	return urls, logging.Wrap(err, "read saved content resource references")
}

type ExportTask struct {
	ID            int64      `json:"id"`
	Kind          string     `json:"kind"`
	TargetID      int64      `json:"target_id"`
	Format        string     `json:"format"`
	StartAt       *time.Time `json:"start_at,omitempty"`
	EndAt         *time.Time `json:"end_at,omitempty"`
	AuthorUIDs    []int64    `json:"author_uids,omitempty" gorm:"serializer:json"`
	SnapshotMaxID int64      `json:"snapshot_max_id"`
	PostCount     int64      `json:"post_count"`
	Status        string     `json:"status" gorm:"index"`
	FileName      string     `json:"-"`
	DownloadName  string     `json:"download_name"`
	FileSize      int64      `json:"file_size"`
	Error         string     `json:"error,omitempty"`
	TraceID       string     `json:"trace_id,omitempty"`
	SourceTraceID string     `json:"source_trace_id,omitempty"`
	CreatedAt     time.Time  `json:"created_at"`
	StartedAt     *time.Time `json:"started_at,omitempty"`
	FinishedAt    *time.Time `json:"finished_at,omitempty"`
	ExpiresAt     *time.Time `json:"expires_at,omitempty" gorm:"index"`
	StatusURL     string     `json:"status_url,omitempty" gorm:"-"`
	DownloadURL   string     `json:"download_url,omitempty" gorm:"-"`
}

func (s *Store) CreateExportTask(ctx context.Context, task *ExportTask) (err error) {
	ctx, span := logging.Start(ctx, "repository.create_export_task")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Create(task).Error, "create export task")
}

func (s *Store) SaveExportTask(ctx context.Context, task *ExportTask) (err error) {
	ctx, span := logging.Start(ctx, "repository.save_export_task")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Save(task).Error, "save export task")
}

func (s *Store) ExportTask(ctx context.Context, id int64) (task ExportTask, err error) {
	ctx, span := logging.Start(ctx, "repository.export_task")
	defer span.End(&err)
	err = s.db.WithContext(ctx).First(&task, id).Error
	return task, logging.Wrap(err, "read export task")
}

func (s *Store) ExportTasks(ctx context.Context, limit int) (items []ExportTask, err error) {
	ctx, span := logging.Start(ctx, "repository.export_tasks")
	defer span.End(&err)
	if limit <= 0 {
		limit = 100
	}
	items = []ExportTask{}
	err = s.db.WithContext(ctx).Order("id DESC").Limit(limit).Find(&items).Error
	return items, logging.Wrap(err, "list export tasks")
}

func (s *Store) NextQueuedExportTask(ctx context.Context) (task ExportTask, found bool, err error) {
	ctx, span := logging.Start(ctx, "repository.next_queued_export_task")
	defer span.End(&err)
	err = s.db.WithContext(ctx).Where("status = ?", "queued").Order("id ASC").First(&task).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return ExportTask{}, false, nil
	}
	if err != nil {
		return ExportTask{}, false, logging.Wrap(err, "read next queued export task")
	}
	return task, true, nil
}

func (s *Store) InterruptExportTasks(ctx context.Context) (err error) {
	ctx, span := logging.Start(ctx, "repository.interrupt_export_tasks")
	defer span.End(&err)
	now := time.Now().UTC()
	err = s.db.WithContext(ctx).Model(&ExportTask{}).
		Where("status IN ?", []string{"queued", "running"}).
		Updates(map[string]any{"status": "interrupted", "finished_at": now, "error": "服务重启，异步导出已中断，请重新发起"}).Error
	return logging.Wrap(err, "mark interrupted export tasks")
}

func (s *Store) ExpiredExportTasks(ctx context.Context, now time.Time) (items []ExportTask, err error) {
	ctx, span := logging.Start(ctx, "repository.expired_export_tasks")
	defer span.End(&err)
	items = []ExportTask{}
	err = s.db.WithContext(ctx).
		Where("status = ? AND expires_at IS NOT NULL AND expires_at <= ?", "completed", now.UTC()).
		Order("id ASC").Find(&items).Error
	return items, logging.Wrap(err, "list expired export tasks")
}

func (s *Store) MarkExportTaskExpired(ctx context.Context, id int64) (err error) {
	ctx, span := logging.Start(ctx, "repository.mark_export_task_expired")
	defer span.End(&err)
	err = s.db.WithContext(ctx).Model(&ExportTask{}).Where("id = ?", id).
		Updates(map[string]any{"status": "expired", "file_name": ""}).Error
	return logging.Wrap(err, "mark export task expired")
}
