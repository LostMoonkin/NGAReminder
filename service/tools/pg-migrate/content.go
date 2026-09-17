package main

import (
	"encoding/json"
	"fmt"
	"io/fs"
	"strconv"
	"strings"
	"time"

	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/repository"
)

func postKey(r row) string {
	if r.text("post_kind") == "topic" {
		return "main"
	}
	if r.number("pid") <= 0 {
		panic(sourceError(r.table, "pid", "reply/comment PID must be positive"))
	}
	return "pid:" + strconv.FormatInt(r.number("pid"), 10)
}

func (m *importer) content() error {
	if err := m.write(&repository.ResourceSettings{ID: 1, DownloadEnabled: m.o.DownloadEnabled}); err != nil {
		return err
	}
	if err := m.walk("assets", func(r row) error {
		v := repository.Resource{URL: r.text("source_url"), MIME: r.text("mime_type"), Size: r.number("size_bytes")}
		if v.URL == "" || v.Size < 0 {
			return sourceError(r.table, "source_url", "invalid asset metadata")
		}
		switch r.text("download_status") {
		case "ready":
			v.Path = r.text("local_relative_path")
			if !fs.ValidPath(v.Path) || strings.Contains(v.Path, `\`) {
				return sourceError(r.table, "local_relative_path", "asset path must be a safe relative path")
			}
			for part := range strings.SplitSeq(v.Path, "/") {
				if strings.HasPrefix(part, ".") {
					return sourceError(r.table, "local_relative_path", "hidden asset paths are unsupported")
				}
			}
			m.report.Converted["asset_paths_preserved_unchecked"]++
		case "pending", "downloading":
			v.Error = "迁移前资源下载未完成，可按需重新下载"
			m.report.Converted["asset_downloads_interrupted"]++
		case "failed":
			v.Error = "旧资源下载失败，详情保留在 PG 快照"
		case "remote_only":
		default:
			return sourceError(r.table, "download_status", "unsupported asset status")
		}
		return m.write(&v)
	}); err != nil {
		return err
	}
	return m.walk("posts", func(r row) error {
		v := repository.Post{ID: m.id(r.table, r.text("id")), TID: r.number("tid"), Key: postKey(r), PID: r.number("pid"), Kind: r.text("post_kind"), Floor: r.number("floor_number"), AuthorUID: r.number("author_uid"), Author: r.text("author_name"), Subject: r.text("subject"), Body: r.text("content_raw"), CreatedAt: r.at("first_seen_at")}
		if v.TID <= 0 {
			return sourceError(r.table, "tid", "TID must be positive")
		}
		switch v.Kind {
		case "topic":
			v.Kind = "main"
			if v.Floor != 0 {
				return sourceError(r.table, "floor_number", "topic floor must be zero")
			}
		case "reply":
			if v.Floor <= 0 {
				return sourceError(r.table, "floor_number", "reply floor must be positive")
			}
		case "comment":
		default:
			return sourceError(r.table, "post_kind", "unsupported post kind")
		}
		var threads []string
		if err := m.db.Raw("SELECT data FROM migration_source WHERE table_name='threads' AND json_extract(data,'$.tid')=?", v.TID).Scan(&threads).Error; err != nil {
			return logging.WithStack(err)
		}
		if len(threads) != 1 {
			return sourceError(r.table, "tid", "missing or duplicate thread")
		}
		if v.Subject == "" {
			v.Subject = decodeRow("threads", threads[0]).text("title")
		}
		if raw := r.data["published_at_unix"]; len(raw) > 0 && string(raw) != "null" {
			t := time.Unix(r.number("published_at_unix"), 0).UTC()
			v.PublishedAt = &t
		}
		if v.Kind == "comment" {
			parent := m.one("posts", "id", r.text("parent_post_id"))
			v.ParentKey = postKey(parent)
			seen := map[string]bool{r.text("id"): true}
			for {
				if seen[parent.text("id")] || parent.number("tid") != v.TID {
					return sourceError(r.table, "parent_post_id", "cyclic or cross-thread parent")
				}
				seen[parent.text("id")] = true
				if parent.text("post_kind") != "comment" {
					v.ParentFloor = parent.number("floor_number")
					break
				}
				parent = m.one("posts", "id", parent.text("parent_post_id"))
			}
			var raw map[string]json.RawMessage
			r.decode("raw_payload", &raw)
			if value := raw["comment_to_id"]; len(value) > 0 && string(value) != "null" {
				if value[0] == '"' {
					if json.Unmarshal(value, &v.CommentToID) != nil {
						return sourceError(r.table, "raw_payload", "invalid comment_to_id")
					}
				} else {
					v.CommentToID = string(value)
				}
			}
		}
		v.SourceURL = fmt.Sprintf("https://bbs.nga.cn/read.php?tid=%d", v.TID)
		if v.Kind != "main" {
			v.SourceURL += fmt.Sprintf("&pid=%d", v.PID)
		}
		seenURLs := map[string]bool{}
		if err := m.related("post_assets", "post_id", r.text("id"), func(link row) error {
			asset := m.one("assets", "id", link.text("asset_id"))
			url := asset.text("source_url")
			if !seenURLs[url] {
				v.Resources = append(v.Resources, url)
				seenURLs[url] = true
			}
			return nil
		}); err != nil {
			return err
		}
		return m.write(&v)
	})
}
