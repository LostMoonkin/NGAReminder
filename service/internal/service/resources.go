package service

import (
	"context"
	"errors"
	"github.com/rs/zerolog"
	"io/fs"
	"ngareminder/service/internal/config"
	"ngareminder/service/internal/infrastructure"
	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/repository"
	"os"
	"sync"
	"time"
)

type Resources struct {
	maxDownloadBytes int64
	databasePath     string
	monitor          *Monitoring
	files            *infrastructure.Assets
	work             sync.Mutex
}
type ResourceScan struct {
	DatabaseBytes   int64                       `json:"database_bytes"`
	AssetsBytes     int64                       `json:"assets_bytes"`
	AssetsCount     int64                       `json:"assets_count"`
	NotFoundCount   int64                       `json:"not_found_count"`
	IgnoredCount    int64                       `json:"ignored_count"`
	Settings        repository.ResourceSettings `json:"settings"`
	Missing         []repository.Resource       `json:"missing"`
	Unreferenced    []repository.Resource       `json:"unreferenced"`
	Files           []infrastructure.AssetFile  `json:"unreferenced_files"`
	Temporary       []infrastructure.AssetFile  `json:"temporary_files"`
	ReferencedFiles map[string]bool             `json:"-"`
}

func (m *Monitoring) Resources() *Resources { return m.resources }
func (r *Resources) SaveSettings(ctx context.Context, enabled bool) (err error) {
	ctx, span := logging.Start(ctx, "service.save_resource_settings")
	defer span.End(&err)
	r.work.Lock()
	defer r.work.Unlock()
	return r.monitor.store.SaveResourceSettings(ctx, &repository.ResourceSettings{ID: 1, DownloadEnabled: enabled})
}
func (r *Resources) download(ctx context.Context, source string) (err error) {
	ctx, span := logging.Start(ctx, "service.download_saved_resource")
	defer span.End(&err)
	item, err := r.monitor.store.Resource(ctx, source)
	if err != nil {
		return err
	}
	if item.Path != "" {
		file, e := r.files.Open(ctx, item.Path)
		if e == nil {
			return logging.Wrap(file.Close(), "close existing resource")
		}
		if !errors.Is(e, fs.ErrNotExist) {
			return e
		}
	}
	if item.Ignored || item.HTTPStatus == 404 {
		return nil
	}
	limit := r.maxDownloadBytes
	if limit == 0 {
		limit = config.DefaultMaxDownloadBytes
	}
	data, mime, downloadErr := r.monitor.notifications.sender.DownloadResource(ctx, source, limit, false)
	if downloadErr == nil {
		item.Path, downloadErr = r.files.Save(ctx, data, mime, source, item.OriginalName)
	}
	if downloadErr == nil {
		item.MIME, item.Size, item.HTTPStatus, item.Error = mime, int64(len(data)), 0, ""
	} else {
		item.HTTPStatus = 0
		var responseErr *infrastructure.ResourceHTTPError
		if errors.As(downloadErr, &responseErr) {
			item.HTTPStatus = responseErr.StatusCode
		}
		item.Error = FailureMessage(downloadErr)
	}
	write, done := context.WithTimeout(context.WithoutCancel(ctx), 5*time.Second)
	defer done()
	err = r.monitor.store.SaveResource(write, &item)
	return errors.Join(downloadErr, err)
}

// 正文已提交后才下载，资源失败只记录结果，不回滚正文或水位。
func (r *Resources) Collect(ctx context.Context, posts []repository.Post) {
	ctx, span := logging.Start(ctx, "service.collect_post_resources")
	var err error
	defer span.End(&err)
	r.work.Lock()
	defer r.work.Unlock()
	settings, err := r.monitor.store.ResourceSettings(ctx)
	if err != nil {
		logging.Error(ctx, err, "Read resource download settings failed", zerolog.ErrorLevel)
		return
	}
	if !settings.DownloadEnabled || !r.monitor.enabled {
		return
	}
	seen := map[string]bool{}
	for _, post := range posts {
		for _, source := range post.Resources {
			if seen[source] {
				continue
			}
			seen[source] = true
			if e := r.download(ctx, source); e != nil {
				logging.Error(ctx, e, "Resource download failed; saved content retained", zerolog.WarnLevel)
			}
			if ctx.Err() != nil {
				return
			}
		}
	}
	zerolog.Ctx(ctx).Info().Int("resource_count", len(seen)).Msg("Post resources processed")
}
func (r *Resources) scan(ctx context.Context) (v ResourceScan, err error) {
	v.Settings, err = r.monitor.store.ResourceSettings(ctx)
	if err != nil {
		return v, err
	}
	urls, err := r.monitor.store.ResourceReferences(ctx)
	if err != nil {
		return v, err
	}
	records, err := r.monitor.store.Resources(ctx)
	if err != nil {
		return v, err
	}
	referenced := map[string]bool{}
	for _, url := range urls {
		referenced[url] = true
	}
	byURL := map[string]repository.Resource{}
	v.ReferencedFiles = map[string]bool{}
	v.Missing = []repository.Resource{}
	v.Unreferenced = []repository.Resource{}
	v.Files = []infrastructure.AssetFile{}
	v.Temporary = []infrastructure.AssetFile{}
	for _, item := range records {
		byURL[item.URL] = item
		if !referenced[item.URL] {
			v.Unreferenced = append(v.Unreferenced, item)
		} else if item.Path != "" {
			v.ReferencedFiles[item.Path] = true
		}
	}
	files, err := r.files.Scan(ctx)
	if err != nil {
		return v, err
	}
	exists := map[string]bool{}
	for _, file := range files {
		v.AssetsBytes += file.Size
		v.AssetsCount++
		exists[file.Path] = true
		if v.ReferencedFiles[file.Path] {
			continue
		}
		if file.Temporary {
			v.Temporary = append(v.Temporary, file)
		} else {
			v.Files = append(v.Files, file)
		}
	}
	for _, url := range urls {
		item := byURL[url]
		item.URL = url
		if item.Path == "" || !exists[item.Path] {
			switch {
			case item.Ignored:
				v.IgnoredCount++
			case item.HTTPStatus == 404:
				v.NotFoundCount++
			default:
				v.Missing = append(v.Missing, item)
			}
		}
	}
	return v, nil
}
func (r *Resources) Scan(ctx context.Context) (v ResourceScan, err error) {
	ctx, span := logging.Start(ctx, "service.scan_resources")
	defer span.End(&err)
	r.work.Lock()
	defer r.work.Unlock()
	v, err = r.scan(ctx)
	if err != nil {
		return v, err
	}
	v.DatabaseBytes, err = infrastructure.SQLiteDiskUsage(ctx, r.databasePath)
	return v, err
}
func selectedMissing(scan ResourceScan, urls []string) ([]string, error) {
	if len(urls) == 0 {
		return nil, InvalidInput("请至少选择一项缺失资源")
	}
	available := make(map[string]bool, len(scan.Missing))
	for _, item := range scan.Missing {
		available[item.URL] = true
	}
	selected := make([]string, 0, len(urls))
	seen := make(map[string]bool, len(urls))
	for _, url := range urls {
		if !available[url] {
			return nil, InvalidInput("所选资源已不再可处理，请刷新后重试")
		}
		if !seen[url] {
			seen[url] = true
			selected = append(selected, url)
		}
	}
	return selected, nil
}

func (r *Resources) Redownload(ctx context.Context, urls []string) (count int, err error) {
	ctx, span := logging.Start(ctx, "service.redownload_missing_resources")
	defer span.End(&err)
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	stop := context.AfterFunc(r.monitor.ctx, cancel)
	defer stop()
	if !r.monitor.enabled {
		return 0, logging.WithStack(ErrBackgroundDisabled)
	}
	r.work.Lock()
	defer r.work.Unlock()
	scan, err := r.scan(ctx)
	if err != nil {
		return 0, err
	}
	selected, err := selectedMissing(scan, urls)
	if err != nil {
		return 0, err
	}
	if !scan.Settings.DownloadEnabled {
		return 0, InvalidInput("请先启用资源下载")
	}
	for _, url := range selected {
		if e := r.download(ctx, url); e != nil {
			logging.Error(ctx, e, "Missing resource download failed", zerolog.WarnLevel)
		} else {
			count++
		}
		if ctx.Err() != nil {
			return count, logging.WithStack(ctx.Err())
		}
	}
	zerolog.Ctx(ctx).Info().Int("selected", len(selected)).Int("downloaded", count).Msg("Selected resource downloads completed")
	return count, nil
}
func (r *Resources) Ignore(ctx context.Context, urls []string) (count int, err error) {
	ctx, span := logging.Start(ctx, "service.ignore_resources")
	defer span.End(&err)
	r.work.Lock()
	defer r.work.Unlock()
	scan, err := r.scan(ctx)
	if err != nil {
		return 0, err
	}
	selected, err := selectedMissing(scan, urls)
	if err != nil {
		return 0, err
	}
	if err = r.monitor.store.IgnoreResources(ctx, selected); err != nil {
		return 0, err
	}
	zerolog.Ctx(ctx).Info().Int("ignored", len(selected)).Msg("Selected resources ignored")
	return len(selected), nil
}
func (r *Resources) Cleanup(ctx context.Context, confirmed bool) (count int, err error) {
	ctx, span := logging.Start(ctx, "service.cleanup_resources")
	defer span.End(&err)
	if !confirmed {
		return 0, InvalidInput("请先扫描资源并确认清理超过 24 小时的无引用文件")
	}
	// 同一把采集锁确保扫描到删除之间不会新增正文引用；资源锁保护并发下载。
	if err = r.monitor.lock(); err != nil {
		return 0, err
	}
	defer r.monitor.work.Unlock()
	r.work.Lock()
	defer r.work.Unlock()
	scan, err := r.scan(ctx)
	if err != nil {
		return 0, err
	}
	for _, file := range append(scan.Files, scan.Temporary...) {
		removed, e := r.files.RemoveUnreferenced(ctx, file.Path, time.Now().Add(-24*time.Hour))
		if e != nil {
			return count, e
		}
		if removed {
			count++
		}
	}
	zerolog.Ctx(ctx).Info().Int("removed", count).Msg("Unreferenced resource cleanup completed")
	return count, nil
}
func (r *Resources) Open(ctx context.Context, name string) (file *os.File, err error) {
	ctx, span := logging.Start(ctx, "service.open_resource")
	defer span.End(&err)
	return r.files.Open(ctx, name)
}
