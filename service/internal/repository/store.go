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
		seedObservations := !tx.Migrator().HasTable(&WatchPost{})
		if e := tx.AutoMigrate(&Account{}, &Watch{}, &Run{}, &Post{}, &FeishuApp{}, &Channel{}, &InboxEvent{}, &EventWatch{}, &WatchPost{}, &Delivery{}); e != nil {
			return e
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
