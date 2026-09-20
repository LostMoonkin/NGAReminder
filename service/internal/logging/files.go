package logging

import (
	"bytes"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/rs/zerolog"
)

// EnableFiles 在配置加载后切换出口，已经建立的 context/span 继续使用同一个脱敏 writer。
func (l *Logger) EnableFiles(directory, timezone string) error {
	l.writer.mu.Lock()
	defer l.writer.mu.Unlock()
	console, ok := l.writer.out.(zerolog.ConsoleWriter)
	if !ok || l.files != nil {
		return WithStack(errors.New("file logging requires an unconfigured console logger"))
	}
	location, err := time.LoadLocation(timezone)
	if err != nil {
		return Wrap(err, "load logging timezone")
	}
	files, err := newDailyWriter(console.Out, directory, location, time.Now)
	if err != nil {
		return err
	}
	l.files, l.writer.out = files, files
	return nil
}

// Close 在最后一条退出日志之后调用；恢复终端出口，关闭失败仍能被打印。
func (l *Logger) Close() error {
	l.writer.mu.Lock()
	defer l.writer.mu.Unlock()
	if l.files == nil {
		return nil
	}
	files := l.files
	l.files, l.writer.out = nil, consoleWriter(files.console, files.location)
	return Wrap(files.close(), "close daily log files")
}

// dailyWriter 的写入、切分和关闭由外层 redactingWriter 的锁串行保护。
type dailyWriter struct {
	console   io.Writer
	directory string
	location  *time.Location
	now       func() time.Time
	date      string
	app, sql  *os.File
}

func newDailyWriter(console io.Writer, directory string, location *time.Location, now func() time.Time) (*dailyWriter, error) {
	if err := os.MkdirAll(directory, 0700); err != nil {
		return nil, Wrap(err, "create log directory")
	}
	w := &dailyWriter{console: console, directory: directory, location: location, now: now}
	if err := w.rotate(now().In(location)); err != nil {
		return nil, errors.Join(err, Wrap(w.close(), "close incomplete log files"))
	}
	return w, nil
}

func (w *dailyWriter) Write(p []byte) (int, error) {
	var event struct {
		Kind  string `json:"log_type"`
		Level string `json:"level"`
	}
	if err := json.Unmarshal(p, &event); err != nil {
		return 0, Wrap(err, "read log routing fields")
	}
	var text bytes.Buffer
	if _, err := consoleWriter(&text, w.location).Write(p); err != nil {
		return 0, Wrap(err, "format file log event")
	}
	now := w.now().In(w.location)
	err := w.rotate(now)
	// 清理旧文件失败不妨碍已打开的当日日志；打开新文件失败则不误写旧日期。
	if w.date == now.Format(time.DateOnly) {
		file := w.app
		if event.Kind == "sql" {
			file = w.sql
		}
		_, writeErr := file.Write(text.Bytes())
		err = errors.Join(err, Wrap(writeErr, "append daily log file"))
	}
	// 文件失败仍尝试终端；返回的错误由外层通过独立终端出口报告。
	if event.Kind != "sql" || event.Level == "error" || event.Level == "fatal" || event.Level == "panic" {
		_, consoleErr := w.console.Write(text.Bytes())
		err = errors.Join(err, Wrap(consoleErr, "write console log"))
	}
	if err != nil {
		return 0, err
	}
	return len(p), nil
}

func (w *dailyWriter) rotate(now time.Time) error {
	date := now.Format(time.DateOnly)
	if date == w.date {
		return nil
	}
	app, err := os.OpenFile(filepath.Join(w.directory, "app-"+date+".log"), os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0600)
	if err != nil {
		return Wrap(err, "open daily application log")
	}
	sql, err := os.OpenFile(filepath.Join(w.directory, "sql-"+date+".log"), os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0600)
	if err != nil {
		return errors.Join(Wrap(err, "open daily SQL log"), Wrap(app.Close(), "close incomplete application log"))
	}
	closeErr := w.close()
	w.app, w.sql, w.date = app, sql, date
	return errors.Join(Wrap(closeErr, "close previous daily log files"), w.cleanup(now))
}

func (w *dailyWriter) cleanup(now time.Time) error {
	entries, err := os.ReadDir(w.directory)
	if err != nil {
		return Wrap(err, "list log directory for retention")
	}
	cutoff := now.AddDate(0, 0, -29).Format(time.DateOnly)
	var result error
	for _, entry := range entries {
		if !entry.Type().IsRegular() {
			continue
		}
		name := entry.Name()
		if !strings.HasPrefix(name, "app-") && !strings.HasPrefix(name, "sql-") {
			continue
		}
		date := strings.TrimSuffix(name[4:], ".log")
		if len(name) != len("app-2006-01-02.log") || !strings.HasSuffix(name, ".log") {
			continue
		}
		if _, err := time.Parse(time.DateOnly, date); err != nil || date >= cutoff {
			continue
		}
		result = errors.Join(result, Wrap(os.Remove(filepath.Join(w.directory, name)), "remove expired log file"))
	}
	return result
}

func (w *dailyWriter) close() error {
	var result error
	for _, file := range []*os.File{w.app, w.sql} {
		if file != nil {
			result = errors.Join(result, file.Close())
		}
	}
	w.app, w.sql = nil, nil
	return result
}
