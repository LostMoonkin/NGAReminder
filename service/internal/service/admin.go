package service

import (
	"context"
	"crypto/sha256"
	"crypto/subtle"
	"errors"
	"time"

	"github.com/rs/zerolog"

	"ngareminder/service/internal/config"
	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/repository"
)

var ErrUnauthorized = errors.New("API token 无效")

type Admin struct {
	store     *repository.Store
	settings  config.Public
	tokenHash [32]byte
	location  *time.Location
}

type Settings struct {
	config.Public
	CurrentTime    string `json:"current_time"`
	DatabaseStatus string `json:"database_status"`
}

func NewAdmin(ctx context.Context, cfg config.Config, store *repository.Store) (admin *Admin, err error) {
	ctx, span := logging.Start(ctx, "service.initialize_admin")
	defer span.End(&err)
	location, err := time.LoadLocation(cfg.Timezone)
	if err != nil {
		return nil, logging.Wrap(err, "加载调度时区")
	}
	zerolog.Ctx(ctx).Info().Bool("background_enabled", cfg.BackgroundEnabled).Str("timezone", cfg.Timezone).Msg("运行设置已加载")
	return &Admin{store, cfg.Public(), sha256.Sum256([]byte(cfg.APIToken)), location}, nil
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
