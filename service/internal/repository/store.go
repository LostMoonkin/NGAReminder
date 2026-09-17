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

type Session struct {
	TokenHash string `gorm:"primaryKey;size:64"`
	ExpiresAt int64  `gorm:"not null;index"`
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
		return nil, logging.Wrap(err, "打开 SQLite")
	}
	pool, err := db.DB()
	if err != nil {
		return nil, logging.Wrap(err, "取得 SQLite 连接")
	}
	pool.SetMaxOpenConns(1)
	pool.SetMaxIdleConns(1)
	defer func() {
		if err != nil {
			err = errors.Join(err, logging.Wrap(pool.Close(), "关闭未初始化的 SQLite"))
		}
	}()
	db = db.WithContext(ctx)
	var id, tables int
	if err = db.Raw("PRAGMA application_id").Scan(&id).Error; err != nil {
		return nil, logging.Wrap(err, "读取 SQLite 标识")
	}
	if err = db.Raw("SELECT count(*) FROM sqlite_master WHERE type = ? AND name NOT LIKE ?", "table", "sqlite_%").Scan(&tables).Error; err != nil {
		return nil, logging.Wrap(err, "读取 SQLite schema")
	}
	if id != applicationID && (id != 0 || tables > 0) {
		return nil, logging.WithStack(errors.New("database_path 指向未识别的数据库；请使用独立空库，旧数据需显式迁移"))
	}
	if err = db.Exec("PRAGMA journal_mode = WAL").Error; err != nil {
		return nil, logging.Wrap(err, "启用 SQLite WAL")
	}
	if err = db.AutoMigrate(&Session{}); err != nil {
		return nil, logging.Wrap(err, "初始化登录会话表")
	}
	if err = db.Exec(fmt.Sprintf("PRAGMA application_id = %d", applicationID)).Error; err != nil {
		return nil, logging.Wrap(err, "写入 SQLite 标识")
	}
	if err = os.Chmod(path, 0600); err != nil {
		return nil, logging.Wrap(err, "设置数据库文件权限")
	}
	return &Store{db: db, pool: pool}, nil
}

func (s *Store) Check(ctx context.Context) (err error) {
	ctx, span := logging.Start(ctx, "repository.check")
	defer span.End(&err)
	var one int
	err = s.db.WithContext(ctx).Raw("SELECT 1").Scan(&one).Error
	return logging.Wrap(err, "SQLite 不可访问")
}

func (s *Store) SaveSession(ctx context.Context, session Session) (err error) {
	ctx, span := logging.Start(ctx, "repository.save_session")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Create(&session).Error, "保存管理员会话")
}

func (s *Store) SessionValid(ctx context.Context, tokenHash string, now int64) (valid bool, err error) {
	ctx, span := logging.Start(ctx, "repository.session_valid")
	defer span.End(&err)
	var count int64
	err = s.db.WithContext(ctx).Model(&Session{}).Where("token_hash = ? AND expires_at > ?", tokenHash, now).Count(&count).Error
	return count == 1, logging.Wrap(err, "查询管理员会话")
}

func (s *Store) DeleteSession(ctx context.Context, tokenHash string) (err error) {
	ctx, span := logging.Start(ctx, "repository.delete_session")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Where("token_hash = ?", tokenHash).Delete(&Session{}).Error, "删除管理员会话")
}

func (s *Store) DeleteExpiredSessions(ctx context.Context, now int64) (err error) {
	ctx, span := logging.Start(ctx, "repository.delete_expired_sessions")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Where("expires_at <= ?", now).Delete(&Session{}).Error, "清理过期会话")
}

func (s *Store) Close(ctx context.Context) (err error) {
	_, span := logging.Start(ctx, "repository.close")
	defer span.End(&err)
	return logging.Wrap(s.pool.Close(), "关闭 SQLite")
}
