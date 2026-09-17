package infrastructure

import (
	"context"
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
			return logging.Wrap(err, "创建数据目录")
		}
		file, openErr := os.CreateTemp(dir, ".write-check-*")
		if openErr != nil {
			return logging.Wrap(openErr, "数据目录不可写")
		}
		closeErr := file.Close()
		removeErr := os.Remove(file.Name())
		if closeErr != nil {
			return logging.Wrap(closeErr, "关闭目录检查文件")
		}
		if removeErr != nil {
			return logging.Wrap(removeErr, "清理目录检查文件")
		}
	}
	zerolog.Ctx(ctx).Info().Str("database_path", databasePath).Str("assets_path", assetsPath).Msg("数据目录就绪")
	return nil
}
