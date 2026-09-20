package repository

import (
	"context"
	"encoding/json"
	"errors"
	"time"

	"gorm.io/gorm"
	"gorm.io/gorm/clause"

	"ngareminder/service/internal/logging"
)

var ErrNotFound = gorm.ErrRecordNotFound

type Account struct {
	ID         int        `json:"-"`
	Cookie     []byte     `json:"-"`
	UIDMasked  string     `json:"uid_masked"`
	FullCookie bool       `json:"full_cookie"`
	Status     string     `json:"status"`
	CheckedAt  *time.Time `json:"checked_at"`
	LastError  string     `json:"last_error"`
}

type Watch struct {
	ID                 int64  `json:"id"`
	TID                int64  `json:"tid" gorm:"column:tid"`
	UID                int64  `json:"uid" gorm:"column:uid"`
	Kind               string `json:"kind" gorm:"default:tid"`
	Label              string `json:"label"`
	Title              string `json:"title"`
	InitMode           string `json:"init_mode"`
	HistoryConcurrency int    `json:"history_concurrency" gorm:"default:1"`
	Paused             bool   `json:"paused"`
	State              string `json:"state"`
	BaselineComplete   bool   `json:"baseline_complete"`
	CursorFloor        int64  `json:"cursor_floor"`
	// 仅在整轮成功后提交；旧库为 0 时只从已有水位补查尾部，不重新扫描历史。
	RemoteRows       int64 `json:"remote_rows" gorm:"default:0"`
	RemoteTotalPages int   `json:"remote_total_pages" gorm:"default:0"`
	// From-now 的原始边界独立于持续前移的游标；重跑不能把未保存内容误当成历史。
	HistoryFloor    int64          `json:"history_floor"`
	HistoryBefore   *time.Time     `json:"history_before"`
	TopicCursor     UserCursor     `json:"topic_cursor" gorm:"serializer:json"`
	ReplyCursor     UserCursor     `json:"reply_cursor" gorm:"serializer:json"`
	IntervalSeconds int            `json:"interval_seconds" gorm:"default:60"`
	IntervalRules   []IntervalRule `json:"interval_rules" gorm:"serializer:json"`
	NoFetchPeriods  []TimeWindow   `json:"no_fetch_periods" gorm:"serializer:json"`
	NextRunAt       *time.Time     `json:"next_run_at"`
	ChannelIDs      []int64        `json:"channel_ids" gorm:"serializer:json"`
	AuthorUIDs      []int64        `json:"author_uids" gorm:"serializer:json"`
	LastRun         *Run           `json:"last_run,omitempty" gorm:"-"`
	NoFetch         bool           `json:"no_fetch" gorm:"-"`
	NoFetchUntil    *time.Time     `json:"no_fetch_until" gorm:"-"`
	CreatedAt       time.Time      `json:"created_at"`
	UpdatedAt       time.Time      `json:"updated_at"`
}

// 用户列表分别按发布时间和 TID/PID 建立水位，同秒内容通过 ID 区分。
type UserCursor struct {
	Timestamp int64 `json:"timestamp"`
	ID        int64 `json:"id"`
}

type TimeWindow struct {
	Weekdays []int  `json:"weekdays"`
	Start    string `json:"start"`
	End      string `json:"end"`
}

type IntervalRule struct {
	TimeWindow
	IntervalSeconds int `json:"interval_seconds"`
}

type Run struct {
	ID            int64      `json:"id"`
	WatchID       int64      `json:"watch_id" gorm:"index"`
	TID           int64      `json:"tid" gorm:"column:tid;index"`
	UID           int64      `json:"uid" gorm:"column:uid"`
	Source        string     `json:"source"`
	Status        string     `json:"status"`
	Silent        bool       `json:"silent"`
	StartedAt     time.Time  `json:"started_at"`
	FinishedAt    *time.Time `json:"finished_at"`
	Pages         int        `json:"pages"`
	Saved         int64      `json:"saved"`
	Error         string     `json:"error"`
	TraceID       string     `json:"trace_id"`
	SourceTraceID string     `json:"source_trace_id"`
}

type Post struct {
	ResourceNames map[string]string `json:"-" gorm:"-"`
	PageNumber    int               `json:"page_number"`
	RawPayload    json.RawMessage   `json:"raw_payload,omitempty" gorm:"type:text"`
	Thread        *Thread           `json:"-" gorm:"-"`
	ID            int64             `json:"id"`
	TID           int64             `json:"tid" gorm:"column:tid;uniqueIndex:post_key;index"`
	Key           string            `json:"key" gorm:"uniqueIndex:post_key"`
	PID           int64             `json:"pid" gorm:"column:pid"`
	Kind          string            `json:"kind"`
	Floor         int64             `json:"floor"`
	ParentKey     string            `json:"parent_key"`
	ParentFloor   int64             `json:"parent_floor"`
	CommentToID   string            `json:"comment_to_id"`
	AuthorUID     int64             `json:"author_uid"`
	Author        string            `json:"author"`
	Subject       string            `json:"subject"`
	Body          string            `json:"body"`
	PublishedAt   *time.Time        `json:"published_at"`
	SourceURL     string            `json:"source_url"`
	Resources     []string          `json:"resources" gorm:"serializer:json"`
	CreatedAt     time.Time         `json:"created_at"`
}

// 事务范围由 service 决定，回调内的具体读写共用 SQLite 连接。
func (s *Store) Transaction(ctx context.Context, fn func(context.Context, *Store) error) (err error) {
	ctx, span := logging.Start(ctx, "repository.transaction")
	defer span.End(&err)
	err = s.db.WithContext(ctx).Transaction(func(tx *gorm.DB) error { return fn(ctx, &Store{db: tx, pool: s.pool}) })
	return logging.Wrap(err, "commit business transaction")
}

func (s *Store) Account(ctx context.Context) (account Account, err error) {
	ctx, span := logging.Start(ctx, "repository.account")
	defer span.End(&err)
	err = s.db.WithContext(ctx).First(&account, 1).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return Account{ID: 1, Status: "unconfigured"}, nil
	}
	return account, logging.Wrap(err, "read NGA account")
}

func (s *Store) SaveAccount(ctx context.Context, account *Account) (err error) {
	ctx, span := logging.Start(ctx, "repository.save_account")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Save(account).Error, "save NGA account")
}

func (s *Store) SetAuthPaused(ctx context.Context, paused bool) (err error) {
	ctx, span := logging.Start(ctx, "repository.set_auth_paused")
	defer span.End(&err)
	from, to := "ready", "auth_paused"
	if !paused {
		from, to = to, from
	}
	return logging.Wrap(s.db.WithContext(ctx).Model(&Watch{}).Where("state = ?", from).Update("state", to).Error, "update watch authentication state")
}

func (s *Store) Watches(ctx context.Context) (watches []Watch, err error) {
	ctx, span := logging.Start(ctx, "repository.watches")
	defer span.End(&err)
	watches = []Watch{}
	err = s.db.WithContext(ctx).Order("id DESC").Find(&watches).Error
	return watches, logging.Wrap(err, "read watches")
}

func (s *Store) Watch(ctx context.Context, id int64) (watch Watch, err error) {
	ctx, span := logging.Start(ctx, "repository.watch")
	defer span.End(&err)
	err = s.db.WithContext(ctx).First(&watch, id).Error
	return watch, logging.Wrap(err, "read watch")
}

func (s *Store) SaveWatch(ctx context.Context, watch *Watch) (err error) {
	ctx, span := logging.Start(ctx, "repository.save_watch")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Save(watch).Error, "save watch")
}

func (s *Store) DeleteWatch(ctx context.Context, id int64) (err error) {
	ctx, span := logging.Start(ctx, "repository.delete_watch")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Delete(&Watch{}, id).Error, "delete watch")
}

func (s *Store) SaveRun(ctx context.Context, run *Run) (err error) {
	ctx, span := logging.Start(ctx, "repository.save_run")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Save(run).Error, "save collection run")
}

func (s *Store) Run(ctx context.Context, id int64) (run Run, err error) {
	ctx, span := logging.Start(ctx, "repository.run")
	defer span.End(&err)
	err = s.db.WithContext(ctx).First(&run, id).Error
	return run, logging.Wrap(err, "read collection run")
}

func (s *Store) Runs(ctx context.Context, tid int64) (runs []Run, err error) {
	ctx, span := logging.Start(ctx, "repository.runs")
	defer span.End(&err)
	runs = []Run{}
	err = s.db.WithContext(ctx).Where("tid = ?", tid).Order("id DESC").Limit(20).Find(&runs).Error
	return runs, logging.Wrap(err, "read recent collection runs")
}

func (s *Store) WatchRuns(ctx context.Context, id int64) (runs []Run, err error) {
	ctx, span := logging.Start(ctx, "repository.watch_runs")
	defer span.End(&err)
	runs = []Run{}
	err = s.db.WithContext(ctx).Where("watch_id = ?", id).Order("id DESC").Limit(20).Find(&runs).Error
	return runs, logging.Wrap(err, "read recent watch runs")
}

func (s *Store) LatestRuns(ctx context.Context) (runs []Run, err error) {
	ctx, span := logging.Start(ctx, "repository.latest_runs")
	defer span.End(&err)
	err = s.db.WithContext(ctx).Where("id IN (?)", s.db.Model(&Run{}).Select("MAX(id)").Group("watch_id")).Find(&runs).Error
	return runs, logging.Wrap(err, "read latest watch results")
}

func (s *Store) InterruptRuns(ctx context.Context) (err error) {
	ctx, span := logging.Start(ctx, "repository.interrupt_runs")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Model(&Run{}).Where("status = ?", "running").Updates(map[string]any{
		"status": "interrupted", "finished_at": time.Now().UTC(), "error": "上次进程中断，请手动重跑",
	}).Error, "mark interrupted collection runs")
}

func (s *Store) InsertPosts(ctx context.Context, posts []Post) (saved int64, err error) {
	ctx, span := logging.Start(ctx, "repository.insert_posts")
	defer span.End(&err)
	seen := map[int64]bool{}
	for _, p := range posts {
		for url, name := range p.ResourceNames {
			item := Resource{URL: url, OriginalName: name}
			if e := s.db.WithContext(ctx).Clauses(clause.OnConflict{Columns: []clause.Column{{Name: "url"}}, DoUpdates: clause.Assignments(map[string]any{"original_name": gorm.Expr("COALESCE(NULLIF(original_name,''), excluded.original_name)")})}).Create(&item).Error; e != nil {
				return 0, logging.Wrap(e, "save resource original name")
			}
		}
		if seen[p.TID] {
			continue
		}
		seen[p.TID] = true
		t := Thread{TID: p.TID, Coverage: "partial", LastSeenAt: time.Now().UTC()}
		if p.Thread != nil && p.Thread.TID > 0 {
			t = *p.Thread
		} else if p.Kind == "main" {
			t.Title, t.AuthorUID, t.AuthorName = p.Subject, p.AuthorUID, p.Author
		}
		if e := s.UpsertThread(ctx, t); e != nil {
			return 0, e
		}
	}
	if len(posts) == 0 {
		return 0, nil
	}
	result := s.db.WithContext(ctx).Clauses(clause.OnConflict{Columns: []clause.Column{{Name: "tid"}, {Name: "key"}}, DoNothing: true}).CreateInBatches(&posts, 50)
	return result.RowsAffected, logging.Wrap(result.Error, "save new posts")
}

func (s *Store) Posts(ctx context.Context, tid int64, page int) (posts []Post, total int64, err error) {
	ctx, span := logging.Start(ctx, "repository.posts")
	defer span.End(&err)
	query := s.db.WithContext(ctx).Model(&Post{}).Where("tid = ?", tid)
	if err = query.Count(&total).Error; err != nil {
		return nil, 0, logging.Wrap(err, "count posts")
	}
	posts = []Post{}
	err = query.Order("CASE WHEN kind = 'comment' THEN parent_floor ELSE floor END, CASE WHEN kind = 'comment' THEN 1 ELSE 0 END, published_at, id").Offset((page - 1) * 50).Limit(50).Find(&posts).Error
	return posts, total, logging.Wrap(err, "read posts")
}
