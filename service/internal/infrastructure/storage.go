package infrastructure

import (
	"context"
	"errors"
	"io/fs"
	"os"
	"path/filepath"

	"github.com/rs/zerolog"

	"ngareminder/service/internal/logging"
)

func PrepareStorage(ctx context.Context, databasePath, assetsPath string) (err error) {
	ctx, span := logging.Start(ctx, "infrastructure.prepare_storage")
	defer span.End(&err)
	for _, dir := range []string{filepath.Dir(databasePath), assetsPath} {
		if err = os.MkdirAll(dir, 0700); err != nil {
			return logging.Wrap(err, "create data directory")
		}
		file, openErr := os.CreateTemp(dir, ".write-check-*")
		if openErr != nil {
			return logging.Wrap(openErr, "data directory is not writable")
		}
		closeErr := file.Close()
		removeErr := os.Remove(file.Name())
		if closeErr != nil {
			return logging.Wrap(closeErr, "close directory probe file")
		}
		if removeErr != nil {
			return logging.Wrap(removeErr, "remove directory probe file")
		}
	}
	zerolog.Ctx(ctx).Info().Str("database_path", databasePath).Str("assets_path", assetsPath).Msg("Data directories are ready")
	return nil
}

func SQLiteDiskUsage(ctx context.Context, databasePath string) (bytes int64, err error) {
	_, span := logging.Start(ctx, "infrastructure.read_sqlite_disk_usage")
	defer span.End(&err)
	for index, name := range []string{databasePath, databasePath + "-wal", databasePath + "-shm"} {
		if err = ctx.Err(); err != nil {
			return 0, logging.WithStack(err)
		}
		info, statErr := os.Stat(name)
		if index > 0 && errors.Is(statErr, fs.ErrNotExist) {
			continue
		}
		if statErr != nil {
			return 0, logging.Wrap(statErr, "inspect SQLite storage file")
		}
		if !info.Mode().IsRegular() {
			return 0, logging.WithStack(errors.New("SQLite storage path is not an ordinary file"))
		}
		bytes += info.Size()
	}
	return bytes, nil
}
