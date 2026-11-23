package infrastructure

import (
	"context"
	"crypto/rand"
	"errors"
	"io/fs"
	"os"
	"strings"

	"ngareminder/service/internal/logging"
)

// Exports 管理持久化异步导出文件，与资源目录隔离，避免被资源 24 小时清理误删。
type Exports struct{ Path string }

func (e *Exports) root() (*os.Root, error) {
	info, err := os.Lstat(e.Path)
	if err != nil {
		return nil, logging.Wrap(err, "inspect export directory")
	}
	if !info.IsDir() || info.Mode()&os.ModeSymlink != 0 {
		return nil, logging.WithStack(errors.New("export root must be an ordinary directory"))
	}
	root, err := os.OpenRoot(e.Path)
	return root, logging.Wrap(err, "open export directory")
}

func validExportFileName(name string, allowTemp bool) bool {
	if name == "" || name == "." || name == ".." || strings.ContainsAny(name, `/\`) {
		return false
	}
	if !allowTemp && strings.HasPrefix(name, ".") {
		return false
	}
	return fs.ValidPath(name)
}

func (e *Exports) CreateTemp(ctx context.Context, extension string) (file *os.File, name string, err error) {
	_, span := logging.Start(ctx, "infrastructure.create_async_export_temp")
	defer span.End(&err)
	root, err := e.root()
	if err != nil {
		return nil, "", err
	}
	defer root.Close()
	name = ".tmp-" + rand.Text() + extension
	file, err = root.OpenFile(name, os.O_RDWR|os.O_CREATE|os.O_EXCL, 0600)
	if err != nil {
		return nil, "", logging.Wrap(err, "create async export temporary file")
	}
	return file, name, nil
}

func (e *Exports) Publish(ctx context.Context, tempName, finalName string) (err error) {
	_, span := logging.Start(ctx, "infrastructure.publish_async_export")
	defer span.End(&err)
	if !strings.HasPrefix(tempName, ".tmp-") || !validExportFileName(tempName, true) || !validExportFileName(finalName, false) {
		return logging.WithStack(errors.New("invalid export file name"))
	}
	root, err := e.root()
	if err != nil {
		return err
	}
	defer root.Close()
	return logging.Wrap(root.Rename(tempName, finalName), "publish async export file")
}

func (e *Exports) Open(ctx context.Context, name string) (file *os.File, info os.FileInfo, err error) {
	_, span := logging.Start(ctx, "infrastructure.open_async_export")
	defer span.End(&err)
	if !validExportFileName(name, false) {
		return nil, nil, logging.WithStack(errors.New("invalid export file name"))
	}
	root, err := e.root()
	if err != nil {
		return nil, nil, err
	}
	defer root.Close()
	info, err = root.Lstat(name)
	if err != nil {
		return nil, nil, logging.Wrap(err, "inspect async export file")
	}
	if info.Mode()&os.ModeSymlink != 0 || !info.Mode().IsRegular() {
		return nil, nil, logging.WithStack(errors.New("async export path is not an ordinary file"))
	}
	file, err = root.Open(name)
	if err != nil {
		return nil, nil, logging.Wrap(err, "open async export file")
	}
	return file, info, nil
}

func (e *Exports) Remove(ctx context.Context, name string) (err error) {
	_, span := logging.Start(ctx, "infrastructure.remove_async_export")
	defer span.End(&err)
	if !validExportFileName(name, true) {
		return logging.WithStack(errors.New("invalid export file name"))
	}
	root, err := e.root()
	if err != nil {
		return err
	}
	defer root.Close()
	info, err := root.Lstat(name)
	if errors.Is(err, fs.ErrNotExist) {
		return nil
	}
	if err != nil {
		return logging.Wrap(err, "inspect async export file for removal")
	}
	if info.Mode()&os.ModeSymlink != 0 || !info.Mode().IsRegular() {
		return logging.WithStack(errors.New("refuse to remove non-regular export entry"))
	}
	err = root.Remove(name)
	if errors.Is(err, fs.ErrNotExist) {
		return nil
	}
	return logging.Wrap(err, "remove async export file")
}

// CleanIncomplete 清理服务异常退出遗留的未完成临时导出文件，不影响已完成的导出文件或符号链接。
func (e *Exports) CleanIncomplete(ctx context.Context) (removed int, err error) {
	_, span := logging.Start(ctx, "infrastructure.clean_incomplete_exports")
	defer span.End(&err)
	root, err := e.root()
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return 0, nil
		}
		return 0, err
	}
	defer root.Close()
	entries, err := fs.ReadDir(root.FS(), ".")
	if err != nil {
		return 0, logging.Wrap(err, "list export directory")
	}
	for _, entry := range entries {
		name := entry.Name()
		if !strings.HasPrefix(name, ".tmp-") || entry.Type()&os.ModeSymlink != 0 || entry.IsDir() {
			continue
		}
		info, statErr := root.Lstat(name)
		if errors.Is(statErr, fs.ErrNotExist) {
			continue
		}
		if statErr != nil {
			return removed, logging.Wrap(statErr, "inspect incomplete export file")
		}
		if info.Mode()&os.ModeSymlink != 0 || !info.Mode().IsRegular() {
			continue
		}
		if removeErr := root.Remove(name); removeErr != nil && !errors.Is(removeErr, fs.ErrNotExist) {
			return removed, logging.Wrap(removeErr, "remove incomplete export file")
		}
		removed++
	}
	return removed, nil
}
