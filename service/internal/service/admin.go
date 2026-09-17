package service

import (
	"context"
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/base64"
	"encoding/hex"
	"errors"
	"time"

	"github.com/rs/zerolog"
	"golang.org/x/crypto/bcrypt"

	"ngareminder/service/internal/config"
	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/repository"
)

const SessionLifetime = 12 * time.Hour

var ErrUnauthorized = errors.New("管理员凭据无效或会话已过期")

type Admin struct {
	store        *repository.Store
	settings     config.Public
	username     string
	passwordHash []byte
	tokenHash    [32]byte
	sessionKey   []byte
	location     *time.Location
}

type Settings struct {
	config.Public
	CurrentTime    string `json:"current_time"`
	DatabaseStatus string `json:"database_status"`
}

func NewAdmin(ctx context.Context, cfg config.Config, store *repository.Store) (admin *Admin, err error) {
	ctx, span := logging.Start(ctx, "service.initialize_admin")
	defer span.End(&err)
	passwordHash, err := bcrypt.GenerateFromPassword([]byte(cfg.AdminPassword), bcrypt.DefaultCost)
	if err != nil {
		return nil, logging.Wrap(err, "准备管理员凭据")
	}
	key, err := base64.StdEncoding.DecodeString(cfg.EncryptionKey)
	if err != nil || len(key) != 32 {
		return nil, logging.WithStack(errors.New("encryption_key 无效"))
	}
	location, err := time.LoadLocation(cfg.Timezone)
	if err != nil {
		return nil, logging.Wrap(err, "加载调度时区")
	}
	// 会话校验值绑定部署密钥和密码；重启保留会话，修改任一凭据会使旧会话失效。
	mac := hmac.New(sha256.New, key)
	_, _ = mac.Write([]byte("admin-session\x00" + cfg.AdminUsername + "\x00" + cfg.AdminPassword))
	zerolog.Ctx(ctx).Info().Bool("background_enabled", cfg.BackgroundEnabled).Str("timezone", cfg.Timezone).Msg("运行设置已加载")
	return &Admin{store, cfg.Public(), cfg.AdminUsername, passwordHash, sha256.Sum256([]byte(cfg.APIToken)), mac.Sum(nil), location}, nil
}

func (a *Admin) Login(ctx context.Context, username, password string) (token string, expires time.Time, err error) {
	ctx, span := logging.Start(ctx, "service.login")
	defer span.End(&err)
	passwordErr := bcrypt.CompareHashAndPassword(a.passwordHash, []byte(password))
	if username != a.username || passwordErr != nil {
		return "", time.Time{}, logging.WithStack(ErrUnauthorized)
	}
	if err = a.store.DeleteExpiredSessions(ctx, time.Now().Unix()); err != nil {
		return "", time.Time{}, err
	}
	token = rand.Text()
	expires = time.Now().UTC().Add(SessionLifetime).Truncate(time.Second)
	if err = a.store.SaveSession(ctx, repository.Session{TokenHash: a.sessionHash(token), ExpiresAt: expires.Unix()}); err != nil {
		return "", time.Time{}, err
	}
	zerolog.Ctx(ctx).Info().Time("expires_at", expires).Msg("管理员登录成功")
	return token, expires, nil
}

func (a *Admin) CheckSession(ctx context.Context, token string) (err error) {
	ctx, span := logging.Start(ctx, "service.check_session")
	defer span.End(&err)
	if token == "" {
		return logging.WithStack(ErrUnauthorized)
	}
	valid, err := a.store.SessionValid(ctx, a.sessionHash(token), time.Now().Unix())
	if err != nil {
		return err
	}
	if !valid {
		return logging.WithStack(ErrUnauthorized)
	}
	return nil
}

func (a *Admin) CheckToken(ctx context.Context, token string) (err error) {
	ctx, span := logging.Start(ctx, "service.check_api_token")
	defer span.End(&err)
	hash := sha256.Sum256([]byte(token))
	if token == "" || subtle.ConstantTimeCompare(hash[:], a.tokenHash[:]) != 1 {
		return logging.WithStack(ErrUnauthorized)
	}
	return nil
}

func (a *Admin) Logout(ctx context.Context, token string) (err error) {
	ctx, span := logging.Start(ctx, "service.logout")
	defer span.End(&err)
	if token != "" {
		err = a.store.DeleteSession(ctx, a.sessionHash(token))
		if err != nil {
			return err
		}
	}
	zerolog.Ctx(ctx).Info().Msg("管理员已退出")
	return nil
}

func (a *Admin) Settings(ctx context.Context) (settings Settings, err error) {
	ctx, span := logging.Start(ctx, "service.settings")
	defer span.End(&err)
	if err = a.store.Check(ctx); err != nil {
		return settings, err
	}
	return Settings{a.settings, time.Now().In(a.location).Format(time.RFC3339), "ok"}, nil
}

func (a *Admin) Ready(ctx context.Context) (err error) {
	ctx, span := logging.Start(ctx, "service.readiness")
	defer span.End(&err)
	return a.store.Check(ctx)
}

func (a *Admin) sessionHash(token string) string {
	mac := hmac.New(sha256.New, a.sessionKey)
	_, _ = mac.Write([]byte(token))
	return hex.EncodeToString(mac.Sum(nil))
}
