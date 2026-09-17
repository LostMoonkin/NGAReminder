package repository

import (
	"context"
	"fmt"
	"time"

	"github.com/rs/zerolog"
	"gorm.io/gorm/logger"

	"ngareminder/service/internal/logging"
)

type sqlLogger struct{ fallback context.Context }

func (l sqlLogger) context(ctx context.Context) context.Context {
	if logging.TraceID(ctx) == "" {
		return l.fallback
	}
	return ctx
}

func (l sqlLogger) LogMode(logger.LogLevel) logger.Interface { return l }

func (l sqlLogger) Info(ctx context.Context, message string, args ...any) {
	zerolog.Ctx(l.context(ctx)).Info().Msgf(message, args...)
}

func (l sqlLogger) Warn(ctx context.Context, message string, args ...any) {
	zerolog.Ctx(l.context(ctx)).Warn().Msgf(message, args...)
}

func (l sqlLogger) Error(ctx context.Context, message string, args ...any) {
	logging.Error(l.context(ctx), logging.WithStack(fmt.Errorf(message, args...)), "GORM internal error", zerolog.ErrorLevel)
}

func (l sqlLogger) Trace(ctx context.Context, begin time.Time, query func() (string, int64), err error) {
	statement, rows := query()
	// 查询属于当前 repository span；错误详情由返回给 handler/startup 的 error 统一记录。
	zerolog.Ctx(l.context(ctx)).Info().Str("event", "sql").Str("sql", statement).
		Int64("rows", rows).Bool("failed", err != nil).Dur("duration_ms", time.Since(begin)).Msg("")
}

func (l sqlLogger) ParamsFilter(_ context.Context, statement string, _ ...any) (string, []any) {
	return statement, nil
}
