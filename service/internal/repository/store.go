package repository

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"net/url"
	"os"
	"time"

	"github.com/libtnb/sqlite"
	"gorm.io/gorm"

	"ngareminder/service/internal/logging"
)

type Store struct {
	db   *gorm.DB
	pool *sql.DB
}

// sqlite v1.2.2 重建表时通过 GORM.DB() 将 *sql.Tx 还原为连接池，再申请连接。
// 包装事务使驱动复用当前连接，避免单连接池自锁；提交和回滚仍由外层事务负责。
type migrationTx struct{ *sql.Tx }

// application_id 标识 Go 数据库，防止配置失误把 Rust/其他 SQLite 当作新库升级。
const applicationID = 0x4e474147

func Open(ctx context.Context, path string) (store *Store, err error) {
	ctx, span := logging.Start(ctx, "repository.open")
	defer span.End(&err)
	dsn := url.URL{Scheme: "file", Path: path}
	dsn.RawQuery = url.Values{"_pragma": {"foreign_keys(1)", "busy_timeout(5000)"}}.Encode()
	db, err := gorm.Open(sqlite.Open(dsn.String()), &gorm.Config{
		Logger: sqlLogger{fallback: ctx}, DisableAutomaticPing: true,
		NowFunc: func() time.Time { return time.Now().UTC() },
	})
	if err != nil {
		return nil, logging.Wrap(err, "open SQLite")
	}
	pool, err := db.DB()
	if err != nil {
		return nil, logging.Wrap(err, "get SQLite connection")
	}
	pool.SetMaxOpenConns(1)
	pool.SetMaxIdleConns(1)
	defer func() {
		if err != nil {
			err = errors.Join(err, logging.Wrap(pool.Close(), "close uninitialized SQLite connection"))
		}
	}()
	db = db.WithContext(ctx)
	var id, tables int
	if err = db.Raw("PRAGMA application_id").Scan(&id).Error; err != nil {
		return nil, logging.Wrap(err, "read SQLite application ID")
	}
	if err = db.Raw("SELECT count(*) FROM sqlite_master WHERE type = ? AND name NOT LIKE ?", "table", "sqlite_%").Scan(&tables).Error; err != nil {
		return nil, logging.Wrap(err, "read SQLite schema")
	}
	if id != applicationID && (id != 0 || tables > 0) {
		return nil, logging.WithStack(errors.New("database_path points to an unrecognized database; use a separate empty database and explicitly migrate existing data"))
	}
	if err = db.Exec("PRAGMA journal_mode = WAL").Error; err != nil {
		return nil, logging.Wrap(err, "enable SQLite WAL")
	}
	if err = db.Exec(fmt.Sprintf("PRAGMA application_id = %d", applicationID)).Error; err != nil {
		return nil, logging.Wrap(err, "write SQLite application ID")
	}
	if err = os.Chmod(path, 0600); err != nil {
		return nil, logging.Wrap(err, "set database file permissions")
	}
	// 阶段 02 的 TID 唯一索引会把所有 UID 监控的 tid=0 视为重复；原子升级为按类型的部分索引。
	if err = db.Transaction(func(tx *gorm.DB) error {
		tx.Statement.ConnPool = &migrationTx{tx.Statement.ConnPool.(*sql.Tx)}
		// 旧索引只有 event/channel 两列；加入独立告警目标后原子重建。
		if tx.Migrator().HasTable(&Delivery{}) && !tx.Migrator().HasColumn(&Delivery{}, "alert_id") {
			if e := tx.Exec("DROP INDEX IF EXISTS delivery_target").Error; e != nil {
				return e
			}
		}
		seedSources := tx.Migrator().HasTable(&EventWatch{}) && !tx.Migrator().HasColumn(&EventWatch{}, "kind")
		seedThreads := !tx.Migrator().HasTable(&Thread{})
		seedObservations := !tx.Migrator().HasTable(&WatchPost{})
		if e := tx.AutoMigrate(&Account{}, &Watch{}, &Run{}, &Post{}, &Thread{}, &FeishuApp{}, &Channel{}, &InboxEvent{}, &EventWatch{}, &WatchPost{}, &Delivery{}, &SystemAlert{}, &BotSettings{}, &BotBinding{}, &BotReceipt{}, &RenewalSettings{}, &RenewalRequest{}, &ResourceSettings{}, &Resource{}, &Backfill{}, &FloorGap{}); e != nil {
			return e
		}
		if seedSources {
			if e := tx.Exec(`UPDATE event_watches SET kind = COALESCE((SELECT kind FROM watches w WHERE w.id = watch_id), '')`).Error; e != nil {
				return e
			}
		}
		if seedThreads {
			// 只修复可由旧运行记录证明的 C02 误停；真实用户不存在、手动暂停及水位不变。
			if e := tx.Exec(`UPDATE watches SET state = CASE WHEN EXISTS (SELECT 1 FROM accounts WHERE status='auth_paused') THEN 'auth_paused' ELSE 'ready' END
                WHERE kind='uid' AND state='missing' AND EXISTS (SELECT 1 FROM runs r WHERE r.id=(SELECT MAX(id) FROM runs WHERE watch_id=watches.id)
                AND r.status='missing' AND r.error='NGA 主题不存在，已停止自动采集')`).Error; e != nil {
				return e
			}
			// 只恢复旧 Go 确实保存的信息；页码/raw 无从推导，保持未知。
			if e := tx.Exec(`INSERT INTO threads (tid, title, author_uid, author_name, coverage, remote_rows, remote_total_pages, first_seen_at, last_seen_at)
                SELECT p.tid, COALESCE(NULLIF(MAX(w.title), ''), NULLIF(MAX(CASE WHEN p.kind = 'main' THEN p.subject END), ''), MAX(p.subject), ''),
                COALESCE(MAX(CASE WHEN p.kind = 'main' THEN p.author_uid END), 0), COALESCE(MAX(CASE WHEN p.kind = 'main' THEN p.author END), ''),
                'partial', COALESCE(MAX(w.remote_rows), 0), COALESCE(MAX(w.remote_total_pages), 0), MIN(p.created_at), MAX(p.created_at) FROM posts p LEFT JOIN watches w ON w.kind = 'tid' AND w.tid = p.tid GROUP BY p.tid`).Error; e != nil {
				return e
			}
			if e := tx.Exec(`INSERT INTO threads (tid, title, coverage, remote_rows, remote_total_pages, first_seen_at, last_seen_at)
                SELECT w.tid, w.title, 'partial', w.remote_rows, w.remote_total_pages, w.created_at, w.updated_at FROM watches w
                WHERE w.kind = 'tid' AND w.baseline_complete = 1 AND NOT EXISTS (SELECT 1 FROM threads t WHERE t.tid = w.tid)`).Error; e != nil {
				return e
			}
		}
		if seedObservations {
			if e := tx.Exec("INSERT INTO watch_posts (watch_id, post_id) SELECT w.id, p.id FROM watches w JOIN posts p ON p.tid = w.tid WHERE w.kind = 'tid'").Error; e != nil {
				return e
			}
		}
		for _, statement := range []string{
			"DROP INDEX IF EXISTS idx_watches_tid",
			"CREATE UNIQUE INDEX IF NOT EXISTS watch_tid ON watches(tid) WHERE kind = 'tid'",
			"CREATE UNIQUE INDEX IF NOT EXISTS watch_uid ON watches(uid) WHERE kind = 'uid'",
		} {
			if e := tx.Exec(statement).Error; e != nil {
				return e
			}
		}
		return nil
	}); err != nil {
		return nil, logging.Wrap(err, "initialize NGA account and monitoring tables")
	}
	return &Store{db: db, pool: pool}, nil
}

func (s *Store) Check(ctx context.Context) (err error) {
	ctx, span := logging.Start(ctx, "repository.check")
	defer span.End(&err)
	var one int
	err = s.db.WithContext(ctx).Raw("SELECT 1").Scan(&one).Error
	return logging.Wrap(err, "SQLite is unavailable")
}

func (s *Store) Close(ctx context.Context) (err error) {
	_, span := logging.Start(ctx, "repository.close")
	defer span.End(&err)
	return logging.Wrap(s.pool.Close(), "close SQLite")
}
