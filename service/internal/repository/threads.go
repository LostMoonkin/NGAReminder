package repository

import (
	"context"
	"fmt"
	"time"

	"gorm.io/gorm/clause"
	"ngareminder/service/internal/logging"
)

type Thread struct {
	FirstSeenAt      time.Time `json:"first_seen_at"`
	TID              int64     `json:"tid" gorm:"column:tid;primaryKey;autoIncrement:false"`
	FID              int64     `json:"fid" gorm:"column:fid"`
	Title            string    `json:"title"`
	ForumName        string    `json:"forum_name"`
	AuthorUID        int64     `json:"author_uid"`
	AuthorName       string    `json:"author_name"`
	Coverage         string    `json:"coverage"`
	RemoteRows       int64     `json:"remote_rows"`
	RemoteTotalPages int       `json:"remote_total_pages"`
	PerPage          int       `json:"per_page"`
	LastSeenAt       time.Time `json:"last_seen_at"`
}

type ThreadSummary struct {
	Thread `gorm:"embedded"`
	Count  int64 `json:"count"`
}

func (s *Store) UpsertThread(ctx context.Context, item Thread) (err error) {
	ctx, span := logging.Start(ctx, "repository.upsert_thread")
	defer span.End(&err)
	if item.TID <= 0 {
		return nil
	}
	if item.FirstSeenAt.IsZero() {
		item.FirstSeenAt = time.Now().UTC()
	}
	if item.LastSeenAt.IsZero() {
		item.LastSeenAt = time.Now().UTC()
	}
	updates := map[string]any{"last_seen_at": item.LastSeenAt}
	// 局部详情仅补齐未知字段；不能把已有完整主题降级或擦掉标题和分页快照。
	for key, value := range map[string]any{"title": item.Title, "fid": item.FID, "forum_name": item.ForumName, "author_uid": item.AuthorUID, "author_name": item.AuthorName} {
		empty := value == "" || value == int64(0)
		if empty {
			continue
		}
		if item.Coverage == "full" {
			updates[key] = value
		} else {
			updates[key] = clause.Expr{SQL: fmt.Sprintf("CASE WHEN %s IS NULL OR %s = '' OR %s = 0 THEN ? ELSE %s END", key, key, key, key), Vars: []any{value}}
		}
	}
	if item.Coverage == "full" {
		updates["coverage"], updates["remote_rows"], updates["remote_total_pages"], updates["per_page"] = "full", item.RemoteRows, item.RemoteTotalPages, item.PerPage
	}
	return logging.Wrap(s.db.WithContext(ctx).Clauses(clause.OnConflict{Columns: []clause.Column{{Name: "tid"}}, DoUpdates: clause.Assignments(updates)}).Create(&item).Error, "save thread metadata")
}

func (s *Store) Thread(ctx context.Context, tid int64) (item Thread, err error) {
	ctx, span := logging.Start(ctx, "repository.thread")
	defer span.End(&err)
	err = s.db.WithContext(ctx).Where("tid = ?", tid).First(&item).Error
	return item, logging.Wrap(err, "read thread metadata")
}

func (s *Store) Threads(ctx context.Context) (items []ThreadSummary, err error) {
	ctx, span := logging.Start(ctx, "repository.threads")
	defer span.End(&err)
	items = []ThreadSummary{}
	err = s.db.WithContext(ctx).Table("threads t").Select("t.*, (SELECT COUNT(*) FROM posts p WHERE p.tid = t.tid) AS count").Order("t.last_seen_at DESC, t.tid DESC").Scan(&items).Error
	return items, logging.Wrap(err, "read saved threads")
}

type UserSummary struct {
	WatchID         int64      `json:"watch_id"`
	UID             int64      `json:"uid" gorm:"column:uid"`
	Username        string     `json:"username"`
	PostCount       int64      `json:"post_count"`
	ThreadCount     int64      `json:"thread_count"`
	LastPublishedAt *time.Time `json:"last_published_at"`
}

func (s *Store) Users(ctx context.Context) (items []UserSummary, err error) {
	ctx, span := logging.Start(ctx, "repository.users")
	defer span.End(&err)
	items = []UserSummary{}
	err = s.db.WithContext(ctx).Raw(`SELECT w.id AS watch_id, w.uid,
		COALESCE(NULLIF(w.title, ''), (SELECT p.author FROM posts p WHERE p.author_uid = w.uid AND p.author != '' ORDER BY p.published_at DESC, p.id DESC LIMIT 1), '') AS username,
		(SELECT COUNT(*) FROM posts p WHERE p.author_uid = w.uid) AS post_count,
		(SELECT COUNT(DISTINCT tid) FROM posts p WHERE p.author_uid = w.uid) AS thread_count,
		(SELECT published_at FROM posts p WHERE p.author_uid = w.uid AND published_at IS NOT NULL ORDER BY published_at DESC LIMIT 1) AS last_published_at
		FROM watches w WHERE w.kind = 'uid' ORDER BY last_published_at DESC, w.id`).Scan(&items).Error
	return items, logging.Wrap(err, "read watched user summary")
}

func (p Post) WebURL() string {
	if p.PID > 0 && p.PageNumber > 0 {
		return fmt.Sprintf("https://bbs.nga.cn/read.php?tid=%d&page=%d#pid%dAnchor", p.TID, p.PageNumber, p.PID)
	}
	return p.SourceURL
}
