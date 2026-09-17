package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"os"
	"os/signal"
	"path/filepath"
	"syscall"
	"time"
	_ "time/tzdata"

	"github.com/rs/zerolog"
	"ngareminder/service/internal/logging"
)

type options struct {
	Output, Source, Schema, Timezone string
	DownloadEnabled                  bool
}

func main() { os.Exit(execute()) }

func execute() (code int) {
	l := logging.New(os.Stderr)
	l.SetSecrets(os.Getenv("NGA_MIGRATE_PG_URL"), os.Getenv("NGA_MIGRATE_OLD_KEY"), os.Getenv("NGA_REMINDER_ENCRYPTION_KEY"))
	ctx, cancel := signal.NotifyContext(l.WithContext(context.Background()), os.Interrupt, syscall.SIGTERM)
	defer cancel()
	ctx, span := logging.Start(ctx, "migration")
	var err error
	defer func() {
		if v := recover(); v != nil {
			err = logging.FromPanic(v)
		}
		if err != nil {
			logging.Error(ctx, err, "Migration failed; do not use partial output", zerolog.ErrorLevel)
			code = 1
		}
		span.End(&err)
	}()
	o := options{}
	f := flag.NewFlagSet("pg-migrate", flag.ContinueOnError)
	f.SetOutput(os.Stderr)
	f.StringVar(&o.Output, "output", "", "New SQLite file; never overwrites an existing file")
	f.StringVar(&o.Source, "source", "", "Previously exported PG JSONL snapshot; omit to export NGA_MIGRATE_PG_URL")
	f.StringVar(&o.Schema, "schema", "public", "Source PostgreSQL application schema")
	f.StringVar(&o.Timezone, "timezone", "Asia/Shanghai", "Go deployment timezone matching the old scheduler offset")
	f.BoolVar(&o.DownloadEnabled, "download-enabled", false, "Preserve the old assets.download_enabled setting")
	if err = f.Parse(os.Args[1:]); errors.Is(err, flag.ErrHelp) {
		err = nil
		return 0
	}
	if err != nil {
		err = logging.WithStack(err)
		return 1
	}
	if f.NArg() != 0 || o.Output == "" {
		err = logging.WithStack(errors.New("specify -output and use environment variables for credentials"))
		return 1
	}
	if _, err = time.LoadLocation(o.Timezone); err != nil {
		err = logging.Wrap(err, "load migration timezone")
		return 1
	}
	o.Output, err = filepath.Abs(o.Output)
	if err != nil {
		err = logging.WithStack(err)
		return 1
	}
	if err = unused(o.Output); err != nil {
		return 1
	}
	if err = unused(o.Output + ".report.json"); err != nil {
		return 1
	}
	if err = os.MkdirAll(filepath.Dir(o.Output), 0700); err != nil {
		err = logging.Wrap(err, "create output directory")
		return 1
	}
	if o.Source == "" {
		o.Source = o.Output + ".pg.jsonl"
		if err = exportPG(ctx, o.Schema, o.Source); err != nil {
			return 1
		}
	}
	if err = convert(ctx, o); err != nil {
		return 1
	}
	fmt.Fprintln(os.Stdout, "SQLite:", o.Output)
	fmt.Fprintln(os.Stdout, "PG snapshot:", o.Source)
	fmt.Fprintln(os.Stdout, "Report:", o.Output+".report.json")
	return 0
}

func unused(path string) error {
	_, err := os.Lstat(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return logging.Wrap(err, "inspect destination")
	}
	return logging.WithStack(fmt.Errorf("destination already exists: %s", path))
}

// 硬链接发布提供“不覆盖”语义；临时文件与输出在同一文件系统。
func publish(temp, destination string) error {
	return logging.Wrap(os.Link(temp, destination), "publish completed output without overwriting")
}
