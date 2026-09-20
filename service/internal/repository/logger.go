package repository

import (
	"context"
	"errors"
	"fmt"
	"time"

	"github.com/rs/zerolog"
	"gorm.io/gorm"
	"gorm.io/gorm/logger"

	"ngareminder/service/internal/logging"
)

type sqlLogger struct{ fallback context.Context }

func (l sqlLogger) context(ctx context.Context) context.Context {
	if logging.TraceID(ctx) == "" {
		ctx = l.fallback
	}
	log := zerolog.Ctx(ctx).With().Str("log_type", "sql").Logger()
	return log.WithContext(ctx)
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
	// SQL 错误保留查询现场；业务边界仍按原调用链记录操作失败。
	ctx = l.context(ctx)
	log := zerolog.Ctx(ctx).With().Str("event", "sql").Str("sql", statement).
		Int64("rows", rows).Bool("failed", err != nil).Dur("duration_ms", time.Since(begin)).Logger()
	if err != nil && !errors.Is(err, gorm.ErrRecordNotFound) {
		logging.Error(log.WithContext(ctx), err, "SQL query failed", zerolog.ErrorLevel)
		return
	}
	log.Info().Msg("")
}

func (l sqlLogger) ParamsFilter(_ context.Context, statement string, _ ...any) (string, []any) {
	return statement, nil
}
