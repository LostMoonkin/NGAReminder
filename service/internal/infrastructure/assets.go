package infrastructure

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io/fs"
	"ngareminder/service/internal/logging"
	"os"
	"path/filepath"
	"strings"
	"time"
)

type Assets struct{ Path string }
type AssetFile struct {
	Path      string    `json:"path"`
	Size      int64     `json:"size"`
	Modified  time.Time `json:"modified"`
	Temporary bool      `json:"temporary"`
}

func (a *Assets) root() (*os.Root, error) {
	info, err := os.Lstat(a.Path)
	if err != nil {
		return nil, logging.Wrap(err, "inspect resource directory")
	}
	if !info.IsDir() || info.Mode()&os.ModeSymlink != 0 {
		return nil, logging.WithStack(errors.New("resource root must be an ordinary directory"))
	}
	root, err := os.OpenRoot(a.Path)
	return root, logging.Wrap(err, "open resource directory")
}

// 文件名来自内容摘要和经过检查的 MIME，不采用远端文件名。
func (a *Assets) Save(ctx context.Context, data []byte, mime string) (name string, err error) {
	_, span := logging.Start(ctx, "infrastructure.save_asset")
	defer span.End(&err)
	ext := map[string]string{"image/png": ".png", "image/jpeg": ".jpg", "image/gif": ".gif", "image/webp": ".webp", "image/bmp": ".bmp", "application/pdf": ".pdf", "application/zip": ".zip", "application/x-rar-compressed": ".rar", "application/x-7z-compressed": ".7z"}[mime]
	if ext == "" {
		return "", logging.WithStack(errors.New("unsupported NGA resource content type"))
	}
	sum := sha256.Sum256(data)
	name = hex.EncodeToString(sum[:]) + ext
	root, err := a.root()
	if err != nil {
		return "", err
	}
	defer root.Close()
	if info, e := root.Lstat(name); e == nil {
		if !info.Mode().IsRegular() {
			return "", logging.WithStack(errors.New("resource destination is not an ordinary file"))
		}
		return name, nil
	} else if !errors.Is(e, fs.ErrNotExist) {
		return "", logging.Wrap(e, "inspect resource destination")
	}
	temp := ".download-" + rand.Text()
	file, err := root.OpenFile(temp, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0600)
	if err != nil {
		return "", logging.Wrap(err, "create resource temporary file")
	}
	defer func() {
		if e := root.Remove(temp); e != nil && !errors.Is(e, fs.ErrNotExist) {
			err = errors.Join(err, logging.Wrap(e, "remove resource temporary file"))
		}
	}()
	_, writeErr := file.Write(data)
	closeErr := file.Close()
	if writeErr != nil || closeErr != nil {
		return "", logging.Wrap(errors.Join(writeErr, closeErr), "write resource file")
	}
	if err = ctx.Err(); err != nil {
		return "", logging.WithStack(err)
	}
	return name, logging.Wrap(root.Rename(temp, name), "publish resource file")
}
func (a *Assets) Open(ctx context.Context, name string) (file *os.File, err error) {
	_, span := logging.Start(ctx, "infrastructure.open_asset")
	defer span.End(&err)
	if name == "" || strings.ContainsAny(name, "/\\") || strings.HasPrefix(name, ".") {
		return nil, logging.WithStack(errors.New("invalid resource file name"))
	}
	root, err := a.root()
	if err != nil {
		return nil, err
	}
	defer root.Close()
	info, err := root.Lstat(name)
	if err != nil {
		return nil, logging.Wrap(err, "inspect resource file")
	}
	if !info.Mode().IsRegular() {
		return nil, logging.WithStack(errors.New("resource is not an ordinary file"))
	}
	file, err = root.Open(name)
	return file, logging.Wrap(err, "open resource file")
}
func (a *Assets) Temp(ctx context.Context, extension string) (file *os.File, err error) {
	_, span := logging.Start(ctx, "infrastructure.create_export_file")
	defer span.End(&err)
	root, err := a.root()
	if err != nil {
		return nil, err
	}
	defer root.Close()
	file, err = root.OpenFile(".export-"+rand.Text()+extension, os.O_RDWR|os.O_CREATE|os.O_EXCL, 0600)
	return file, logging.Wrap(err, "create export temporary file")
}
func (a *Assets) RemoveTemp(ctx context.Context, file *os.File) (err error) {
	_, span := logging.Start(ctx, "infrastructure.remove_export_file")
	defer span.End(&err)
	name := filepath.Base(file.Name())
	closeErr := file.Close()
	if !strings.HasPrefix(name, ".export-") {
		return logging.WithStack(errors.New("invalid export temporary file name"))
	}
	root, err := a.root()
	if err != nil {
		return errors.Join(err, logging.Wrap(closeErr, "close export temporary file"))
	}
	defer root.Close()
	return logging.Wrap(errors.Join(closeErr, root.Remove(name)), "remove export temporary file")
}
func (a *Assets) Scan(ctx context.Context) (items []AssetFile, err error) {
	_, span := logging.Start(ctx, "infrastructure.scan_assets")
	defer span.End(&err)
	root, err := a.root()
	if err != nil {
		return nil, err
	}
	defer root.Close()
	items = []AssetFile{}
	err = fs.WalkDir(root.FS(), ".", func(path string, entry fs.DirEntry, e error) error {
		if e != nil {
			return e
		}
		if e = ctx.Err(); e != nil {
			return e
		}
		if entry.Type()&os.ModeSymlink != 0 || entry.IsDir() {
			return nil
		}
		info, e := entry.Info()
		if e != nil {
			return e
		}
		if !info.Mode().IsRegular() {
			return nil
		}
		items = append(items, AssetFile{path, info.Size(), info.ModTime().UTC(), strings.HasPrefix(entry.Name(), ".export-") || strings.HasPrefix(entry.Name(), ".download-") || strings.HasPrefix(path, ".tmp/")})
		return nil
	})
	return items, logging.Wrap(err, "scan resource files")
}
func (a *Assets) RemoveUnreferenced(ctx context.Context, name string, olderThan time.Time) (removed bool, err error) {
	_, span := logging.Start(ctx, "infrastructure.remove_unreferenced_asset")
	defer span.End(&err)
	if !fs.ValidPath(name) || name == "." {
		return false, logging.WithStack(fmt.Errorf("invalid resource cleanup path"))
	}
	root, err := a.root()
	if err != nil {
		return false, err
	}
	defer root.Close()
	// 每个路径分量均拒绝符号链接；删除前重新检查类型和时间。
	parts := strings.Split(name, "/")
	for i := range parts {
		info, e := root.Lstat(strings.Join(parts[:i+1], "/"))
		if errors.Is(e, fs.ErrNotExist) {
			return false, nil
		}
		if e != nil {
			return false, logging.Wrap(e, "inspect cleanup path")
		}
		if info.Mode()&os.ModeSymlink != 0 {
			return false, nil
		}
		if i == len(parts)-1 && (!info.Mode().IsRegular() || !info.ModTime().Before(olderThan)) {
			return false, nil
		}
	}
	err = root.Remove(name)
	return err == nil, logging.Wrap(err, "remove unreferenced resource file")
}
