package repository

import (
	"context"
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
	ID               int64  `json:"id"`
	TID              int64  `json:"tid" gorm:"column:tid;uniqueIndex"`
	Label            string `json:"label"`
	Title            string `json:"title"`
	InitMode         string `json:"init_mode"`
	Paused           bool   `json:"paused"`
	State            string `json:"state"`
	BaselineComplete bool   `json:"baseline_complete"`
	CursorFloor      int64  `json:"cursor_floor"`
	// From-now 的原始边界独立于持续前移的游标；重跑不能把未保存内容误当成历史。
	HistoryFloor  int64      `json:"history_floor"`
	HistoryBefore *time.Time `json:"history_before"`
	CreatedAt     time.Time  `json:"created_at"`
	UpdatedAt     time.Time  `json:"updated_at"`
}

type Run struct {
	ID            int64      `json:"id"`
	WatchID       int64      `json:"watch_id" gorm:"index"`
	TID           int64      `json:"tid" gorm:"column:tid;index"`
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
	ID          int64      `json:"id"`
	TID         int64      `json:"tid" gorm:"column:tid;uniqueIndex:post_key;index"`
	Key         string     `json:"key" gorm:"uniqueIndex:post_key"`
	PID         int64      `json:"pid" gorm:"column:pid"`
	Kind        string     `json:"kind"`
	Floor       int64      `json:"floor"`
	ParentKey   string     `json:"parent_key"`
	ParentFloor int64      `json:"parent_floor"`
	CommentToID string     `json:"comment_to_id"`
	AuthorUID   int64      `json:"author_uid"`
	Author      string     `json:"author"`
	Subject     string     `json:"subject"`
	Body        string     `json:"body"`
	PublishedAt *time.Time `json:"published_at"`
	SourceURL   string     `json:"source_url"`
	Resources   []string   `json:"resources" gorm:"serializer:json"`
	CreatedAt   time.Time  `json:"created_at"`
}

type ThreadSummary struct {
	TID   int64  `json:"tid" gorm:"column:tid"`
	Title string `json:"title"`
	Count int64  `json:"count"`
}

// 事务范围由 service 决定，回调内的具体读写共用 SQLite 连接。
func (s *Store) Transaction(ctx context.Context, fn func(context.Context, *Store) error) (err error) {
	ctx, span := logging.Start(ctx, "repository.transaction")
	defer span.End(&err)
	err = s.db.WithContext(ctx).Transaction(func(tx *gorm.DB) error { return fn(ctx, &Store{db: tx, pool: s.pool}) })
	return logging.Wrap(err, "提交业务事务")
}

func (s *Store) Account(ctx context.Context) (account Account, err error) {
	ctx, span := logging.Start(ctx, "repository.account")
	defer span.End(&err)
	err = s.db.WithContext(ctx).First(&account, 1).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return Account{ID: 1, Status: "unconfigured"}, nil
	}
	return account, logging.Wrap(err, "读取 NGA 账号")
}

func (s *Store) SaveAccount(ctx context.Context, account *Account) (err error) {
	ctx, span := logging.Start(ctx, "repository.save_account")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Save(account).Error, "保存 NGA 账号")
}

func (s *Store) SetAuthPaused(ctx context.Context, paused bool) (err error) {
	ctx, span := logging.Start(ctx, "repository.set_auth_paused")
	defer span.End(&err)
	from, to := "ready", "auth_paused"
	if !paused {
		from, to = to, from
	}
	return logging.Wrap(s.db.WithContext(ctx).Model(&Watch{}).Where("state = ?", from).Update("state", to).Error, "更新监控认证状态")
}

func (s *Store) Watches(ctx context.Context) (watches []Watch, err error) {
	ctx, span := logging.Start(ctx, "repository.watches")
	defer span.End(&err)
	watches = []Watch{}
	err = s.db.WithContext(ctx).Order("id DESC").Find(&watches).Error
	return watches, logging.Wrap(err, "读取监控列表")
}

func (s *Store) Watch(ctx context.Context, id int64) (watch Watch, err error) {
	ctx, span := logging.Start(ctx, "repository.watch")
	defer span.End(&err)
	err = s.db.WithContext(ctx).First(&watch, id).Error
	return watch, logging.Wrap(err, "读取监控")
}

func (s *Store) SaveWatch(ctx context.Context, watch *Watch) (err error) {
	ctx, span := logging.Start(ctx, "repository.save_watch")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Save(watch).Error, "保存监控")
}

func (s *Store) DeleteWatch(ctx context.Context, id int64) (err error) {
	ctx, span := logging.Start(ctx, "repository.delete_watch")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Delete(&Watch{}, id).Error, "删除监控")
}

func (s *Store) SaveRun(ctx context.Context, run *Run) (err error) {
	ctx, span := logging.Start(ctx, "repository.save_run")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Save(run).Error, "保存采集运行")
}

func (s *Store) Run(ctx context.Context, id int64) (run Run, err error) {
	ctx, span := logging.Start(ctx, "repository.run")
	defer span.End(&err)
	err = s.db.WithContext(ctx).First(&run, id).Error
	return run, logging.Wrap(err, "读取采集运行")
}

func (s *Store) Runs(ctx context.Context, tid int64) (runs []Run, err error) {
	ctx, span := logging.Start(ctx, "repository.runs")
	defer span.End(&err)
	runs = []Run{}
	err = s.db.WithContext(ctx).Where("tid = ?", tid).Order("id DESC").Limit(20).Find(&runs).Error
	return runs, logging.Wrap(err, "读取最近采集运行")
}

func (s *Store) InterruptRuns(ctx context.Context) (err error) {
	ctx, span := logging.Start(ctx, "repository.interrupt_runs")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Model(&Run{}).Where("status = ?", "running").Updates(map[string]any{
		"status": "interrupted", "finished_at": time.Now().UTC(), "error": "上次进程中断，请手动重跑",
	}).Error, "标记中断的采集")
}

func (s *Store) InsertPosts(ctx context.Context, posts []Post) (saved int64, err error) {
	ctx, span := logging.Start(ctx, "repository.insert_posts")
	defer span.End(&err)
	if len(posts) == 0 {
		return 0, nil
	}
	result := s.db.WithContext(ctx).Clauses(clause.OnConflict{Columns: []clause.Column{{Name: "tid"}, {Name: "key"}}, DoNothing: true}).CreateInBatches(&posts, 50)
	return result.RowsAffected, logging.Wrap(result.Error, "保存新帖子")
}

func (s *Store) Posts(ctx context.Context, tid int64, page int) (posts []Post, total int64, err error) {
	ctx, span := logging.Start(ctx, "repository.posts")
	defer span.End(&err)
	query := s.db.WithContext(ctx).Model(&Post{}).Where("tid = ?", tid)
	if err = query.Count(&total).Error; err != nil {
		return nil, 0, logging.Wrap(err, "统计帖子")
	}
	posts = []Post{}
	err = query.Order("CASE WHEN kind = 'comment' THEN parent_floor ELSE floor END, CASE WHEN kind = 'comment' THEN 1 ELSE 0 END, published_at, id").Offset((page - 1) * 50).Limit(50).Find(&posts).Error
	return posts, total, logging.Wrap(err, "读取帖子")
}

func (s *Store) Threads(ctx context.Context) (threads []ThreadSummary, err error) {
	ctx, span := logging.Start(ctx, "repository.threads")
	defer span.End(&err)
	threads = []ThreadSummary{}
	err = s.db.WithContext(ctx).Model(&Post{}).Select("tid, MAX(CASE WHEN kind = 'main' THEN subject ELSE '' END) AS title, COUNT(*) AS count").Group("tid").Order("MAX(id) DESC").Scan(&threads).Error
	return threads, logging.Wrap(err, "读取已保存主题")
}
