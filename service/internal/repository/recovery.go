package repository

import (
	"context"
	"gorm.io/gorm/clause"
	"ngareminder/service/internal/logging"
	"time"
)

type Backfill struct {
	ID            int64      `json:"id"`
	WatchID       int64      `json:"watch_id" gorm:"index"`
	UID           int64      `json:"uid" gorm:"column:uid"`
	StartAt       time.Time  `json:"start_at"`
	EndAt         time.Time  `json:"end_at"`
	Status        string     `json:"status"`
	Pages         int        `json:"pages"`
	Candidates    int        `json:"candidates"`
	Saved         int64      `json:"saved"`
	Error         string     `json:"error"`
	TraceID       string     `json:"trace_id"`
	SourceTraceID string     `json:"source_trace_id"`
	FinishedAt    *time.Time `json:"finished_at"`
}
type FloorGap struct {
	WatchID     int64     `json:"watch_id" gorm:"primaryKey"`
	Floor       int64     `json:"floor" gorm:"primaryKey"`
	Status      string    `json:"status"`
	FirstSeen   time.Time `json:"first_seen"`
	Deadline    time.Time `json:"deadline"`
	NextAttempt int       `json:"next_attempt"`
}
type GapSummary struct {
	Pending  int64 `json:"pending"`
	Resolved int64 `json:"resolved"`
	Expired  int64 `json:"expired"`
}

func (s *Store) SaveBackfill(ctx context.Context, v *Backfill) (err error) {
	ctx, span := logging.Start(ctx, "repository.save_backfill")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Save(v).Error, "save UID reply backfill")
}
func (s *Store) Backfill(ctx context.Context, id int64) (v Backfill, err error) {
	ctx, span := logging.Start(ctx, "repository.backfill")
	defer span.End(&err)
	err = s.db.WithContext(ctx).First(&v, id).Error
	return v, logging.Wrap(err, "read backfill")
}
func (s *Store) Backfills(ctx context.Context, watchID int64) (items []Backfill, err error) {
	ctx, span := logging.Start(ctx, "repository.watch_backfills")
	defer span.End(&err)
	items = []Backfill{}
	err = s.db.WithContext(ctx).Where("watch_id = ?", watchID).Order("id DESC").Limit(20).Find(&items).Error
	return items, logging.Wrap(err, "read watch backfills")
}
func (s *Store) InterruptBackfills(ctx context.Context) (err error) {
	ctx, span := logging.Start(ctx, "repository.interrupt_backfills")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Model(&Backfill{}).Where("status = ?", "running").Updates(map[string]any{"status": "interrupted", "finished_at": time.Now().UTC(), "error": "上次回填中断，请重新提交相同范围"}).Error, "interrupt old backfills")
}
func (s *Store) FloorGaps(ctx context.Context, watchID int64) (items []FloorGap, err error) {
	ctx, span := logging.Start(ctx, "repository.floor_gaps")
	defer span.End(&err)
	items = []FloorGap{}
	q := s.db.WithContext(ctx)
	if watchID > 0 {
		q = q.Where("watch_id = ?", watchID)
	}
	err = q.Order("watch_id, floor").Find(&items).Error
	return items, logging.Wrap(err, "read floor gaps")
}
func (s *Store) AddFloorGaps(ctx context.Context, items []FloorGap) (err error) {
	ctx, span := logging.Start(ctx, "repository.add_floor_gaps")
	defer span.End(&err)
	if len(items) == 0 {
		return nil
	}
	return logging.Wrap(s.db.WithContext(ctx).Clauses(clause.OnConflict{DoNothing: true}).CreateInBatches(items, 200).Error, "record new floor gaps")
}
func (s *Store) SaveFloorGap(ctx context.Context, v *FloorGap) (err error) {
	ctx, span := logging.Start(ctx, "repository.save_floor_gap")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Save(v).Error, "save floor gap state")
}

// 调度读到的快照可能已被采集解决或重置删除，只过期仍未改变的记录。
func (s *Store) ExpireFloorGap(ctx context.Context, v FloorGap) (err error) {
	ctx, span := logging.Start(ctx, "repository.expire_floor_gap")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Model(&FloorGap{}).
		Where("watch_id = ? AND floor = ? AND status = ? AND deadline = ? AND next_attempt = ?", v.WatchID, v.Floor, "pending", v.Deadline, v.NextAttempt).
		Update("status", "expired").Error, "expire unchanged floor gap")
}
func (s *Store) ClearFloorGaps(ctx context.Context, watchID int64) (err error) {
	ctx, span := logging.Start(ctx, "repository.clear_floor_gaps")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Where("watch_id = ?", watchID).Delete(&FloorGap{}).Error, "clear invalidated floor gaps")
}
