package service

import (
	"archive/zip"
	"bufio"
	"cmp"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"github.com/rs/zerolog"
	"io"
	"ngareminder/service/internal/content"
	"ngareminder/service/internal/infrastructure"
	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/repository"
	"os"
	"path"
	"slices"
	"sort"
	"strconv"
	"strings"
	"time"
	"unicode"
)

type SavedContent struct {
	TID      int64             `json:"tid"`
	UID      int64             `json:"uid"`
	Posts    []repository.Post `json:"posts"`
	Page     int               `json:"page"`
	Total    int64             `json:"total"`
	Runs     []repository.Run  `json:"runs"`
	Timezone string            `json:"-"`
}

type ExportInput struct {
	Format     string
	StartAt    string
	EndAt      string
	AuthorUIDs []string
}

type ExportResult struct {
	File     *os.File
	Task     *repository.ExportTask
	Format   string
	Filename string
}

func contentFilter(kind string, id int64) (repository.ContentFilter, error) {
	if id <= 0 {
		return repository.ContentFilter{}, InvalidInput("TID/UID 必须为正整数")
	}
	switch kind {
	case "threads":
		return repository.ContentFilter{TID: id}, nil
	case "users":
		return repository.ContentFilter{UID: id}, nil
	}
	return repository.ContentFilter{}, InvalidInput("请选择主题或用户内容")
}
func (m *Monitoring) Content(ctx context.Context, kind string, id int64, page int) (v SavedContent, err error) {
	ctx, span := logging.Start(ctx, "service.saved_content")
	defer span.End(&err)
	filter, err := contentFilter(kind, id)
	if err != nil {
		return v, err
	}
	if page < 1 {
		return v, InvalidInput("页码必须为正整数")
	}
	v.TID, v.UID, v.Page, v.Timezone = filter.TID, filter.UID, page, m.location.String()
	var maxID int64
	v.Total, maxID, err = m.store.ContentStats(ctx, filter)
	if err != nil {
		return v, err
	}
	v.Posts, err = m.store.ContentBatch(ctx, filter, (page-1)*50, 50, maxID)
	if err != nil {
		return v, err
	}
	if filter.TID > 0 {
		v.Runs, err = m.store.Runs(ctx, id)
	}
	return
}

func (m *Monitoring) exportFilter(kind string, id int64, input ExportInput) (repository.ContentFilter, error) {
	filter, err := contentFilter(kind, id)
	if err != nil {
		return filter, err
	}
	filter.AuthorUIDs, err = parseExportAuthorUIDs(kind, input.AuthorUIDs)
	if err != nil {
		return filter, err
	}
	filter.StartAt, err = m.parseExportTime(input.StartAt, "开始时间")
	if err != nil {
		return filter, err
	}
	filter.EndAt, err = m.parseExportTime(input.EndAt, "结束时间")
	if err != nil {
		return filter, err
	}
	if filter.StartAt != nil && filter.EndAt != nil && filter.StartAt.After(*filter.EndAt) {
		return filter, InvalidInput("开始时间不能晚于结束时间")
	}
	return filter, nil
}

func parseExportAuthorUIDs(kind string, rawValues []string) ([]int64, error) {
	hasInput := false
	seen := map[int64]bool{}
	uids := []int64{}
	for _, raw := range rawValues {
		trimmed := strings.TrimSpace(raw)
		if trimmed == "" {
			continue
		}
		hasInput = true
		if kind == "users" {
			return nil, InvalidInput("仅主题导出支持按作者 UID 过滤")
		}
		parts := strings.FieldsFunc(trimmed, func(r rune) bool {
			return r == ',' || r == '，' || unicode.IsSpace(r)
		})
		if len(parts) == 0 {
			return nil, InvalidInput("作者 UID 必须为正整数，多个 UID 请用逗号或空白分隔")
		}
		for _, part := range parts {
			uid, err := strconv.ParseInt(part, 10, 64)
			if err != nil || uid <= 0 {
				return nil, InvalidInput("作者 UID 必须为正整数，多个 UID 请用逗号或空白分隔")
			}
			if !seen[uid] {
				seen[uid] = true
				uids = append(uids, uid)
			}
		}
	}
	if !hasInput {
		return nil, nil
	}
	sort.Slice(uids, func(i, j int) bool { return uids[i] < uids[j] })
	return uids, nil
}

func exportExtension(format string) (string, error) {
	switch format {
	case "markdown":
		return ".md", nil
	case "zip":
		return ".zip", nil
	default:
		return "", InvalidInput("导出格式为 markdown 或 zip")
	}
}

func exportDownloadName(kind string, id int64, format string) string {
	ext := "md"
	if format == "zip" {
		ext = "zip"
	}
	return fmt.Sprintf("%s-%d.%s", kind, id, ext)
}

func (m *Monitoring) StartExport(ctx context.Context, kind string, id int64, input ExportInput) (result ExportResult, err error) {
	ctx, span := logging.Start(ctx, "service.start_export")
	defer span.End(&err)
	filter, err := m.exportFilter(kind, id, input)
	if err != nil {
		return result, err
	}
	ext, err := exportExtension(input.Format)
	if err != nil {
		return result, err
	}
	total, maxID, err := m.store.ContentStats(ctx, filter)
	if err != nil {
		return result, err
	}
	if total == 0 {
		return result, InvalidInput("没有可导出的已保存内容")
	}
	result.Format = input.Format
	result.Filename = exportDownloadName(kind, id, input.Format)
	if total <= m.exports.threshold {
		file, err := m.exportSync(ctx, kind, id, input.Format, ext, filter, total, maxID)
		if err != nil {
			return result, err
		}
		result.File = file
		return result, nil
	}
	task := repository.ExportTask{
		Kind:          kind,
		TargetID:      id,
		Format:        input.Format,
		StartAt:       filter.StartAt,
		EndAt:         filter.EndAt,
		AuthorUIDs:    filter.AuthorUIDs,
		SnapshotMaxID: maxID,
		PostCount:     total,
		Status:        "queued",
		DownloadName:  result.Filename,
		SourceTraceID: logging.TraceID(ctx),
		CreatedAt:     time.Now().UTC(),
	}
	if err = m.store.CreateExportTask(ctx, &task); err != nil {
		return result, err
	}
	zerolog.Ctx(ctx).Info().Int64("task_id", task.ID).Str("kind", kind).Int64("target_id", id).
		Str("format", input.Format).Int64("posts", total).Int("author_uid_count", len(filter.AuthorUIDs)).
		Int64("threshold", m.exports.threshold).Msg("Async export task created")
	m.exports.Wake()
	result.Task = &task
	return result, nil
}

func (m *Monitoring) parseExportTime(raw, label string) (*time.Time, error) {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return nil, nil
	}
	hasSubsecond := len(raw) > len("2006-01-02") && strings.ContainsAny(raw[len("2006-01-02"):], ".,")
	parsed, err := time.Parse(time.RFC3339, raw)
	if err != nil {
		parsed, err = time.ParseInLocation("2006-01-02T15:04:05", raw, m.location)
	}
	if err != nil {
		parsed, err = time.ParseInLocation("2006-01-02T15:04", raw, m.location)
	}
	if err != nil || hasSubsecond || parsed.Nanosecond() != 0 {
		return nil, InvalidInput(label + "必须精确到秒，可使用 RFC3339 或 YYYY-MM-DDTHH:mm:ss")
	}
	parsed = parsed.UTC()
	return &parsed, nil
}

// 导出只持有一批正文及一个资源文件；临时文件由 HTTP 入口在发送完成/取消后清理。
func (m *Monitoring) Export(ctx context.Context, kind string, id int64, input ExportInput) (file *os.File, err error) {
	ctx, span := logging.Start(ctx, "service.export_content")
	defer span.End(&err)
	filter, err := m.exportFilter(kind, id, input)
	if err != nil {
		return nil, err
	}
	ext, err := exportExtension(input.Format)
	if err != nil {
		return nil, err
	}
	total, maxID, err := m.store.ContentStats(ctx, filter)
	if err != nil {
		return nil, err
	}
	if total == 0 {
		return nil, InvalidInput("没有可导出的已保存内容")
	}
	return m.exportSync(ctx, kind, id, input.Format, ext, filter, total, maxID)
}

func (m *Monitoring) exportSync(ctx context.Context, kind string, id int64, format, extension string, filter repository.ContentFilter, total, maxID int64) (file *os.File, err error) {
	file, err = m.resources.files.Temp(ctx, extension)
	if err != nil {
		return nil, err
	}
	success := false
	defer func() {
		if !success {
			err = errors.Join(err, m.resources.files.RemoveTemp(ctx, file))
			file = nil
		}
	}()
	assetCount, err := m.writeExport(ctx, file, kind, id, format, filter, total, maxID)
	if err != nil {
		return file, err
	}
	if _, err = file.Seek(0, io.SeekStart); err != nil {
		return file, logging.Wrap(err, "rewind export file")
	}
	log := zerolog.Ctx(ctx).Info().Str("kind", kind).Int64("target_id", id).Str("format", format).Int64("posts", total).Int("assets", assetCount).Int("author_uid_count", len(filter.AuthorUIDs))
	if filter.StartAt != nil {
		log = log.Time("start_at", *filter.StartAt)
	}
	if filter.EndAt != nil {
		log = log.Time("end_at", *filter.EndAt)
	}
	log.Msg("Content export ready")
	success = true
	return file, nil
}

const exportBatchSize = 500

func exportSortFloor(row repository.ExportRowKey) int64 {
	if row.Kind == "comment" {
		return row.ParentFloor
	}
	return row.Floor
}

func exportIsComment(row repository.ExportRowKey) int {
	if row.Kind == "comment" {
		return 1
	}
	return 0
}

func zipMethodForAsset(name string) uint16 {
	switch strings.ToLower(path.Ext(name)) {
	case ".jpg", ".jpeg", ".png", ".gif", ".webp", ".avif",
		".mp3", ".m4a", ".aac", ".ogg", ".opus",
		".mp4", ".mkv", ".webm", ".mov",
		".zip", ".7z", ".rar", ".gz", ".bz2", ".xz", ".zst", ".pdf":
		return zip.Store
	default:
		return zip.Deflate
	}
}

func (m *Monitoring) writeExport(ctx context.Context, file *os.File, kind string, id int64, format string, filter repository.ContentFilter, total, maxID int64) (int, error) {
	index, err := m.store.ExportIndex(ctx, filter, maxID)
	if err != nil {
		return 0, err
	}
	refSet := make(map[content.PostReference]struct{}, len(index))
	for _, row := range index {
		if row.Kind == "main" {
			refSet[content.PostReference{TID: row.TID, PID: 0}] = struct{}{}
		}
		if row.PID > 0 {
			refSet[content.PostReference{TID: row.TID, PID: row.PID}] = struct{}{}
		}
	}
	slices.SortFunc(index, func(a, b repository.ExportRowKey) int {
		if c := cmp.Compare(a.TID, b.TID); c != 0 {
			return c
		}
		if c := cmp.Compare(exportSortFloor(a), exportSortFloor(b)); c != 0 {
			return c
		}
		if c := cmp.Compare(exportIsComment(a), exportIsComment(b)); c != 0 {
			return c
		}
		return cmp.Compare(a.ID, b.ID)
	})

	fileBuf := bufio.NewWriterSize(file, 256*1024)
	writer := io.Writer(fileBuf)
	var archive *zip.Writer
	var mdBuf *bufio.Writer
	var resourcePaths map[string]string
	var assetReader *infrastructure.AssetReader
	if format == "zip" {
		resourcePaths, err = m.store.ResourcePaths(ctx)
		if err != nil {
			return 0, err
		}
		assetReader, err = m.resources.files.OpenReader(ctx)
		if err != nil {
			return 0, err
		}
		defer assetReader.Close()
		archive = zip.NewWriter(fileBuf)
		entry, e := archive.Create("content.md")
		if e != nil {
			return 0, logging.Wrap(e, "create Markdown ZIP entry")
		}
		mdBuf = bufio.NewWriterSize(entry, 256*1024)
		writer = mdBuf
	}

	currentTID := int64(0)
	assets := map[string]bool{}
	checkedAssets := map[string]bool{}
	batchIDs := make([]int64, 0, exportBatchSize)
	for start := 0; start < len(index); start += exportBatchSize {
		if err = ctx.Err(); err != nil {
			return 0, logging.WithStack(err)
		}
		end := start + exportBatchSize
		if end > len(index) {
			end = len(index)
		}
		batchIDs = batchIDs[:0]
		for _, row := range index[start:end] {
			batchIDs = append(batchIDs, row.ID)
		}
		posts, e := m.store.ExportPostsByIDs(ctx, batchIDs)
		if e != nil {
			return 0, e
		}
		byID := make(map[int64]repository.Post, len(posts))
		for _, post := range posts {
			byID[post.ID] = post
		}
		for _, postID := range batchIDs {
			post, ok := byID[postID]
			if !ok {
				continue
			}
			body := content.ParseMarkdown(post.Body, post.TID)
			var links map[content.PostReference]string
			if len(body.References) > 0 {
				links = make(map[content.PostReference]string, len(body.References))
				for _, ref := range body.References {
					if _, exists := refSet[ref]; exists {
						links[ref] = "#" + ref.Anchor()
					} else {
						links[ref] = ref.URL()
					}
				}
			}
			if currentTID != post.TID {
				currentTID = post.TID
				title := post.Subject
				if title == "" {
					title = fmt.Sprintf("TID %d", post.TID)
				}
				if _, err = fmt.Fprintf(writer, "\n# %s · TID %d\n\n", content.EscapeMarkdown(title), post.TID); err != nil {
					return 0, logging.Wrap(err, "write export topic heading")
				}
			}
			var replacements map[string]string
			if archive != nil && len(post.Resources) > 0 {
				for _, source := range post.Resources {
					localPath := resourcePaths[source]
					if localPath == "" {
						continue
					}
					available, checked := checkedAssets[localPath]
					if !checked {
						if checkErr := assetReader.Check(localPath); checkErr != nil {
							logging.Error(ctx, checkErr, "Export resource unavailable; using remote link", zerolog.WarnLevel)
							checkedAssets[localPath] = false
							continue
						}
						checkedAssets[localPath] = true
						available = true
					}
					if !available {
						continue
					}
					if replacements == nil {
						replacements = make(map[string]string, len(post.Resources))
					}
					replacements[source] = "assets/" + localPath
					assets[localPath] = true
				}
			}
			if err = writePost(writer, post, replacements, body, links); err != nil {
				return 0, err
			}
		}
	}
	if archive != nil {
		if err = mdBuf.Flush(); err != nil {
			return 0, logging.Wrap(err, "flush Markdown ZIP entry")
		}
		metadata, e := archive.Create("metadata.json")
		if e != nil {
			return 0, logging.Wrap(e, "create export metadata")
		}
		meta := map[string]any{"version": 1, "kind": kind, "id": id, "post_count": int64(len(index)), "snapshot_max_id": maxID, "created_at": time.Now().UTC()}
		if filter.StartAt != nil {
			meta["start_at"] = filter.StartAt.UTC().Format(time.RFC3339)
		}
		if filter.EndAt != nil {
			meta["end_at"] = filter.EndAt.UTC().Format(time.RFC3339)
		}
		if len(filter.AuthorUIDs) > 0 {
			meta["author_uids"] = filter.AuthorUIDs
		}
		if e = json.NewEncoder(metadata).Encode(meta); e != nil {
			return 0, logging.Wrap(e, "write export metadata")
		}
		// 只保留唯一文件名集合；按确定顺序使用固定缓冲流式复制，不把资源载入内存。
		assetNames := make([]string, 0, len(assets))
		for name := range assets {
			assetNames = append(assetNames, name)
		}
		sort.Strings(assetNames)
		copyBuf := make([]byte, 64*1024)
		for _, name := range assetNames {
			if err = ctx.Err(); err != nil {
				return 0, logging.WithStack(err)
			}
			asset, e := assetReader.Open(name)
			if e != nil {
				return 0, e
			}
			entry, e := archive.CreateHeader(&zip.FileHeader{
				Name:   "assets/" + name,
				Method: zipMethodForAsset(name),
			})
			if e == nil {
				_, e = copyContextBuf(ctx, entry, asset, copyBuf)
			}
			closeErr := asset.Close()
			if e != nil || closeErr != nil {
				return 0, logging.Wrap(errors.Join(e, closeErr), "copy export resource")
			}
		}
		if err = archive.Close(); err != nil {
			return 0, logging.Wrap(err, "finish ZIP export")
		}
	}
	if err = fileBuf.Flush(); err != nil {
		return 0, logging.Wrap(err, "flush export file")
	}
	return len(assets), nil
}
func writePost(writer io.Writer, post repository.Post, replacements map[string]string, body content.MarkdownBody, links map[content.PostReference]string) error {
	when := "未知时间"
	if post.PublishedAt != nil {
		when = post.PublishedAt.UTC().Format(time.RFC3339)
	}
	floor := fmt.Sprintf("%d 楼", post.Floor)
	if post.Kind == "main" {
		floor = "主楼"
	} else if post.Kind == "comment" {
		floor = fmt.Sprintf("%d 楼下的评论（父帖 %s）", post.ParentFloor, post.ParentKey)
	}
	source := content.SafeURL(post.SourceURL)
	anchor := content.PostReference{TID: post.TID, PID: post.PID}.Anchor()
	if _, err := fmt.Fprintf(writer, "<a id=\"%s\"></a>\n\n", anchor); err != nil {
		return logging.Wrap(err, "write exported post anchor")
	}
	if post.Kind == "main" && post.PID != 0 {
		if _, err := fmt.Fprintf(writer, "<a id=\"%s\"></a>\n\n", (content.PostReference{TID: post.TID}).Anchor()); err != nil {
			return logging.Wrap(err, "write exported main post anchor")
		}
	}
	_, err := fmt.Fprintf(writer, "## %s · %s\n\nUID %d · PID %d · %s\n\n%s\n\n[原帖](%s)\n\n", content.EscapeMarkdown(floor), content.EscapeMarkdown(content.AnonymousName(post.Author)), post.AuthorUID, post.PID, when, body.Render(content.MarkdownResourceAliases(post.Resources, replacements), links), content.Destination(source))
	if err != nil {
		return logging.Wrap(err, "write exported post")
	}
	for _, raw := range post.Resources {
		target := content.SafeURL(raw)
		if target == "" {
			continue
		}
		if local, ok := replacements[raw]; ok {
			target = local
		}
		if _, err = fmt.Fprintf(writer, "[资源](%s)\n\n", content.Destination(target)); err != nil {
			return logging.Wrap(err, "write exported resource link")
		}
	}
	return nil
}
func copyContext(ctx context.Context, to io.Writer, from io.Reader) (written int64, err error) {
	return copyContextBuf(ctx, to, from, make([]byte, 32*1024))
}

func copyContextBuf(ctx context.Context, to io.Writer, from io.Reader, buffer []byte) (written int64, err error) {
	for {
		if err = ctx.Err(); err != nil {
			return written, logging.WithStack(err)
		}
		n, readErr := from.Read(buffer)
		if n > 0 {
			sent, e := to.Write(buffer[:n])
			written += int64(sent)
			if e != nil {
				return written, logging.Wrap(e, "write content stream")
			}
			if sent != n {
				return written, logging.WithStack(io.ErrShortWrite)
			}
		}
		if errors.Is(readErr, io.EOF) {
			return written, nil
		}
		if readErr != nil {
			return written, logging.Wrap(readErr, "read content stream")
		}
	}
}
func (m *Monitoring) SendExport(ctx context.Context, to io.Writer, file *os.File) (err error) {
	ctx, span := logging.Start(ctx, "service.send_export")
	defer span.End(&err)
	_, err = copyContext(ctx, to, file)
	return err
}
func (m *Monitoring) RemoveExport(ctx context.Context, file *os.File) (err error) {
	ctx, span := logging.Start(ctx, "service.remove_export")
	defer span.End(&err)
	return m.resources.files.RemoveTemp(ctx, file)
}

func (m *Monitoring) Users(ctx context.Context) (items []repository.UserSummary, err error) {
	ctx, span := logging.Start(ctx, "service.user_summary")
	defer span.End(&err)
	return m.store.Users(ctx)
}
