package logging

import (
	"context"
	"crypto/rand"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"runtime"
	"strings"
	"sync"
	"time"

	"github.com/rs/zerolog"
)

// Logger 只补充项目需要的脱敏；业务代码从 context 取得原生 zerolog。
type Logger struct {
	zerolog.Logger
	writer *redactingWriter
}

func New(out io.Writer) *Logger {
	w := &redactingWriter{out: out, replacements: strings.NewReplacer()}
	return &Logger{Logger: zerolog.New(w).Level(zerolog.InfoLevel).With().Timestamp().Logger(), writer: w}
}

func (l *Logger) SetSecrets(secrets ...string) {
	pairs := make([]string, 0, len(secrets)*4)
	for _, secret := range secrets {
		if secret == "" {
			continue
		}
		encoded, _ := json.Marshal(secret)
		pairs = append(pairs, string(encoded[1:len(encoded)-1]), "[REDACTED]", secret, "[REDACTED]")
	}
	l.writer.mu.Lock()
	defer l.writer.mu.Unlock()
	l.writer.replacements = strings.NewReplacer(pairs...)
}

type redactingWriter struct {
	mu           sync.Mutex
	out          io.Writer
	replacements *strings.Replacer
}

func (w *redactingWriter) Write(p []byte) (int, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	_, err := io.WriteString(w.out, w.replacements.Replace(string(p)))
	if err != nil {
		return 0, err
	}
	return len(p), nil
}

type spanKey struct{}

type Span struct {
	TraceID string
	ID      string
	started time.Time
	logger  zerolog.Logger
}

func Start(ctx context.Context, operation string) (context.Context, *Span) {
	traceID, parentID := rand.Text(), ""
	if parent, ok := ctx.Value(spanKey{}).(*Span); ok {
		traceID, parentID = parent.TraceID, parent.ID
	}
	id := rand.Text()
	l := zerolog.Ctx(ctx).With().Reset().Str("trace_id", traceID).Str("span_id", id).
		Str("parent_span_id", parentID).Str("operation", operation).Logger()
	s := &Span{TraceID: traceID, ID: id, started: time.Now(), logger: l}
	l.Info().Str("event", "start").Msg(operation)
	return l.WithContext(context.WithValue(ctx, spanKey{}, s)), s
}

// 直接 defer span.End(&err)，在返回时读取最终错误；panic 在最早的层捕获栈后继续传播。
func (s *Span) End(err *error) {
	result := "ok"
	if value := recover(); value != nil {
		failure := FromPanic(value)
		s.logger.Info().Str("event", "end").Str("result", "panic").Dur("duration_ms", time.Since(s.started)).Msg("")
		panic(failure)
	}
	if err != nil && *err != nil {
		result = "error"
	}
	s.logger.Info().Str("event", "end").Str("result", result).Dur("duration_ms", time.Since(s.started)).Msg("")
}

func TraceID(ctx context.Context) string {
	if s, ok := ctx.Value(spanKey{}).(*Span); ok {
		return s.TraceID
	}
	return ""
}

type frame struct {
	Function string `json:"func"`
	File     string `json:"file"`
	Line     int    `json:"line"`
}

type stackError struct {
	err   error
	stack []frame
}

func (e *stackError) Error() string { return e.err.Error() }
func (e *stackError) Unwrap() error { return e.err }

func captureStack() []frame {
	pcs := make([]uintptr, 32)
	for {
		n := runtime.Callers(3, pcs)
		if n < len(pcs) {
			pcs = pcs[:n]
			break
		}
		pcs = make([]uintptr, len(pcs)*2)
	}
	frames := runtime.CallersFrames(pcs)
	var result []frame
	for {
		f, more := frames.Next()
		result = append(result, frame{f.Function, f.File, f.Line})
		if !more {
			return result
		}
	}
}

func WithStack(err error) error {
	if err == nil {
		return nil
	}
	var existing *stackError
	if errors.As(err, &existing) {
		return err
	}
	return &stackError{err: err, stack: captureStack()}
}

func Wrap(err error, message string) error {
	if err == nil {
		return nil
	}
	return fmt.Errorf("%s: %w", message, WithStack(err))
}

type panicError struct{ *stackError }

func FromPanic(value any) error {
	if existing, ok := value.(*panicError); ok {
		return existing
	}
	err, ok := value.(error)
	if !ok {
		err = fmt.Errorf("%v", value)
	}
	return &panicError{&stackError{err: fmt.Errorf("panic: %w", err), stack: captureStack()}}
}

func causes(err error) []string {
	if err == nil {
		return nil
	}
	result := []string{err.Error()}
	if joined, ok := err.(interface{ Unwrap() []error }); ok {
		for _, child := range joined.Unwrap() {
			result = append(result, causes(child)...)
		}
	} else {
		result = append(result, causes(errors.Unwrap(err))...)
	}
	return result
}

func Error(ctx context.Context, err error, message string, level zerolog.Level) {
	if err == nil {
		return
	}
	err = WithStack(err)
	var captured *stackError
	var panicFailure *panicError
	var stack []frame
	if errors.As(err, &panicFailure) {
		stack = panicFailure.stack
	} else if errors.As(err, &captured) {
		stack = captured.stack
	}
	zerolog.Ctx(ctx).WithLevel(level).Err(err).Strs("causes", causes(err)).Interface("stack", stack).Msg(message)
}

type errorWriter struct{ ctx context.Context }

func (w errorWriter) Write(p []byte) (int, error) {
	Error(w.ctx, WithStack(errors.New(strings.TrimSpace(string(p)))), "http.server", zerolog.ErrorLevel)
	return len(p), nil
}

func StandardErrors(ctx context.Context) io.Writer { return errorWriter{ctx} }
