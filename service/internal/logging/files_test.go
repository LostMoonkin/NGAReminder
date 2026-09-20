package logging

import (
	"bytes"
	"context"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/rs/zerolog"
)

func readLog(t *testing.T, path string) string {
	t.Helper()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	return string(data)
}

func TestDailyFileRoutingAndRedaction(t *testing.T) {
	var stdout bytes.Buffer
	log := NewConsole(&stdout)
	log.SetSecrets("private-value")
	ctx, span := Start(log.WithContext(context.Background()), "test.files")
	dir := t.TempDir()
	if err := log.EnableFiles(dir, "Asia/Shanghai"); err != nil {
		t.Fatal(err)
	}
	defer log.Close()
	// 切换出口前建立的 span 也必须进入文件。
	zerolog.Ctx(ctx).Info().Msg("normal private-value")
	sql := zerolog.Ctx(ctx).With().Str("log_type", "sql").Str("event", "sql").Str("sql", "SELECT ?").Logger()
	sql.Info().Msg("query-success")
	sql.Warn().Msg("query-warning")
	Error(sql.WithContext(ctx), errors.New("query-error private-value"), "SQL query failed", zerolog.ErrorLevel)
	span.End(nil)
	date := log.files.date
	app := readLog(t, filepath.Join(dir, "app-"+date+".log"))
	queries := readLog(t, filepath.Join(dir, "sql-"+date+".log"))
	for _, text := range []string{stdout.String(), app, queries} {
		if strings.Contains(text, "private-value") || strings.Contains(text, "\x1b[") || strings.HasPrefix(text, "{") {
			t.Fatal("log output leaked credentials, colors, or JSON formatting")
		}
		if !strings.Contains(text, "trace_id="+span.TraceID) {
			t.Fatal("log output lost trace correlation")
		}
	}
	if !strings.Contains(app, "normal [REDACTED]") || strings.Contains(app, "log_type=sql") {
		t.Fatal("application file did not isolate SQL events")
	}
	if strings.Contains(stdout.String(), "query-success") || strings.Contains(stdout.String(), "query-warning") || !strings.Contains(stdout.String(), "ERR SQL query failed") {
		t.Fatal("console SQL filtering failed")
	}
	for _, value := range []string{"query-success", "query-warning", "query-error [REDACTED]", "stack=", "causes=", "TestDailyFileRoutingAndRedaction", "sql=", "SELECT ?"} {
		if !strings.Contains(queries, value) {
			t.Fatalf("SQL file lost %s", value)
		}
	}
	if strings.Contains(queries, "normal") {
		t.Fatal("ordinary event leaked into SQL file")
	}
	appFile, sqlFile := log.files.app, log.files.sql
	if err := log.Close(); err != nil {
		t.Fatal(err)
	}
	for _, file := range []*os.File{appFile, sqlFile} {
		if _, err := file.WriteString("after-close"); !errors.Is(err, os.ErrClosed) {
			t.Fatal("daily file descriptor remained open")
		}
	}
}

func TestDailyRolloverAppendAndRetention(t *testing.T) {
	dir := t.TempDir()
	location, err := time.LoadLocation("Asia/Shanghai")
	if err != nil {
		t.Fatal(err)
	}
	// UTC 尚未跨天，本地时区已到午夜。
	now := time.Date(2026, 9, 20, 15, 59, 59, 0, time.UTC)
	for _, name := range []string{"app-2026-08-21.log", "sql-2026-08-21.log", "app-2026-08-22.log", "sql-2026-08-22.log", "app-2026-09-20.log", "sql-2026-09-20.log", "notes.log", "app-2026-02-30.log", "app-2026-08-01.log.bak", "other-2026-08-01.log"} {
		if err := os.WriteFile(filepath.Join(dir, name), []byte("existing\n"), 0600); err != nil {
			t.Fatal(err)
		}
	}
	if err := os.Mkdir(filepath.Join(dir, "app-2026-01-01.log"), 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink("notes.log", filepath.Join(dir, "sql-2026-01-01.log")); err != nil {
		t.Fatal(err)
	}
	w, err := newDailyWriter(io.Discard, dir, location, func() time.Time { return now })
	if err != nil {
		t.Fatal(err)
	}
	defer w.close()
	for _, prefix := range []string{"app", "sql"} {
		if _, err := os.Stat(filepath.Join(dir, prefix+"-2026-08-21.log")); !errors.Is(err, os.ErrNotExist) {
			t.Fatal("startup did not remove expired log")
		}
		if _, err := os.Stat(filepath.Join(dir, prefix+"-2026-08-22.log")); err != nil {
			t.Fatal("retention deleted the thirtieth calendar day")
		}
	}
	write := func(message string) {
		t.Helper()
		for _, kind := range []string{"app", "sql"} {
			if _, err := w.Write([]byte(`{"level":"info","time":"` + now.Format(time.RFC3339) + `","log_type":"` + kind + `","message":"` + message + `"}`)); err != nil {
				t.Fatal(err)
			}
		}
	}
	write("before-midnight")
	if err := w.close(); err != nil {
		t.Fatal(err)
	}
	w, err = newDailyWriter(io.Discard, dir, location, func() time.Time { return now })
	if err != nil {
		t.Fatal(err)
	}
	defer w.close()
	write("same-day-restart")
	now = now.Add(time.Second)
	write("after-midnight")
	for _, prefix := range []string{"app", "sql"} {
		previous := readLog(t, filepath.Join(dir, prefix+"-2026-09-20.log"))
		if !strings.HasPrefix(previous, "existing\n") || !strings.Contains(previous, "before-midnight") || !strings.Contains(previous, "same-day-restart") || strings.Contains(previous, "after-midnight") {
			t.Fatal("same-day append or rollover failed")
		}
		current := readLog(t, filepath.Join(dir, prefix+"-2026-09-21.log"))
		if !strings.Contains(current, "2026-09-21 00:00:00 +08:00") || !strings.Contains(current, "after-midnight") {
			t.Fatal("file date or text timestamp ignored configured timezone")
		}
		if _, err := os.Stat(filepath.Join(dir, prefix+"-2026-08-22.log")); !errors.Is(err, os.ErrNotExist) {
			t.Fatal("rollover did not expire the oldest retained day")
		}
	}
	for _, name := range []string{"notes.log", "app-2026-02-30.log", "app-2026-08-01.log.bak", "other-2026-08-01.log", "app-2026-01-01.log", "sql-2026-01-01.log"} {
		if _, err := os.Lstat(filepath.Join(dir, name)); err != nil {
			t.Fatalf("retention removed unrelated entry %s", name)
		}
	}
}

func TestConcurrentFileLogsAndWriteFailure(t *testing.T) {
	dir := t.TempDir()
	var stdout bytes.Buffer
	log := NewConsole(&stdout)
	if err := log.EnableFiles(dir, "UTC"); err != nil {
		t.Fatal(err)
	}
	defer log.Close()
	var group sync.WaitGroup
	for i := 0; i < 8; i++ {
		group.Go(func() {
			for j := 0; j < 50; j++ {
				log.Info().Msg("concurrent-event")
			}
		})
	}
	group.Wait()
	file := readLog(t, filepath.Join(dir, "app-"+log.files.date+".log"))
	if file != stdout.String() || strings.Count(file, "concurrent-event\n") != 400 {
		t.Fatal("concurrent events were lost or interleaved")
	}
	if err := log.files.app.Close(); err != nil {
		t.Fatal(err)
	}
	_, err := log.files.Write([]byte(`{"level":"info","message":"file-failed-console-visible"}`))
	if err == nil || !strings.Contains(stdout.String(), "file-failed-console-visible") {
		t.Fatal("file write failure was swallowed or suppressed console output")
	}
	log.Info().Msg("failed-file-event")
	for _, value := range []string{"ERR Log file output failed", "stack=", "causes="} {
		if !strings.Contains(stdout.String(), value) {
			t.Fatalf("file failure diagnostic lost %s", value)
		}
	}
	blocked := filepath.Join(dir, "not-a-directory")
	if err := os.WriteFile(blocked, nil, 0600); err != nil {
		t.Fatal(err)
	}
	if err := NewConsole(io.Discard).EnableFiles(blocked, "UTC"); err == nil {
		t.Fatal("unusable log directory was accepted")
	}
}
