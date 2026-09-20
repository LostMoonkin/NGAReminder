package repository

import (
	"context"
	"errors"
	"gorm.io/gorm"
	"ngareminder/service/internal/logging"
)

type ContentFilter struct{ TID, UID int64 }

func (s *Store) contentQuery(ctx context.Context, f ContentFilter) *gorm.DB {
	q := s.db.WithContext(ctx).Model(&Post{})
	if f.TID > 0 {
		q = q.Where("tid = ?", f.TID)
	}
	if f.UID > 0 {
		q = q.Where("author_uid = ?", f.UID)
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
func (s *Store) ResourceReferences(ctx context.Context) (urls []string, err error) {
	ctx, span := logging.Start(ctx, "repository.resource_references")
	defer span.End(&err)
	err = s.db.WithContext(ctx).Raw("SELECT DISTINCT j.value FROM posts p, json_each(p.resources) j WHERE j.type = 'text' ORDER BY j.value").Scan(&urls).Error
	return urls, logging.Wrap(err, "read saved content resource references")
}
