package main

import (
	"fmt"
	"slices"
	"sort"
	"strconv"
	"strings"
	"time"

	"ngareminder/service/internal/repository"
)

type oldWindow struct {
	Days     []string `json:"days"`
	Start    string   `json:"start_time"`
	End      string   `json:"end_time"`
	Interval int      `json:"interval"`
}

func window(r row, old oldWindow) repository.TimeWindow {
	result := repository.TimeWindow{}
	for _, day := range old.Days {
		switch strings.ToLower(strings.TrimSpace(day)) {
		case "weekdays":
			result.Weekdays = append(result.Weekdays, 1, 2, 3, 4, 5)
		case "weekends":
			result.Weekdays = append(result.Weekdays, 6, 7)
		case "monday", "mon":
			result.Weekdays = append(result.Weekdays, 1)
		case "tuesday", "tue", "tues":
			result.Weekdays = append(result.Weekdays, 2)
		case "wednesday", "wed":
			result.Weekdays = append(result.Weekdays, 3)
		case "thursday", "thu", "thur", "thurs":
			result.Weekdays = append(result.Weekdays, 4)
		case "friday", "fri":
			result.Weekdays = append(result.Weekdays, 5)
		case "saturday", "sat":
			result.Weekdays = append(result.Weekdays, 6)
		case "sunday", "sun":
			result.Weekdays = append(result.Weekdays, 7)
		default:
			panic(sourceError(r.table, "schedule", "unsupported weekday"))
		}
	}
	if len(result.Weekdays) == 0 {
		panic(sourceError(r.table, "schedule", "empty weekdays"))
	}
	sort.Ints(result.Weekdays)
	result.Weekdays = slices.Compact(result.Weekdays)
	parse := func(s string, end bool) string {
		h, minute, ok := strings.Cut(s, ":")
		hour, e1 := strconv.Atoi(h)
		min, e2 := strconv.Atoi(minute)
		if !ok || e1 != nil || e2 != nil || hour < 0 || hour > 24 || min < 0 || min > 59 || (hour == 24 && (!end || min != 0)) {
			panic(sourceError(r.table, "schedule", "invalid time of day"))
		}
		return fmt.Sprintf("%02d:%02d", hour%24, min)
	}
	result.Start, result.End = parse(old.Start, false), parse(old.End, true)
	return result
}

func (m *importer) watches() error {
	if err := m.walk("watch_targets", func(r row) error {
		id := m.id(r.table, r.text("id"))
		if r.timestamp("deleted_at") != nil {
			m.report.Converted["deleted_watches_archived"]++
			return nil
		}
		v := repository.Watch{ID: id, Label: r.text("target_name"), Title: r.text("target_name"), InitMode: "full", State: "ready", BaselineComplete: r.yes("baseline_completed"), HistoryFloor: -1, IntervalSeconds: int(r.number("interval_seconds")), NextRunAt: r.timestamp("next_run_at"), CreatedAt: r.at("created_at"), UpdatedAt: r.at("updated_at")}
		if v.IntervalSeconds < 30 || v.IntervalSeconds > 86400 {
			return sourceError(r.table, "interval_seconds", "invalid collection interval")
		}
		if r.number("target_id") <= 0 {
			return sourceError(r.table, "target_id", "target must be positive")
		}
		pause := r.text("pause_reason")
		v.Paused = (!r.yes("enabled") && pause != "auth") || pause == "user" || pause == "error"
		if pause == "auth" {
			v.State = "auth_paused"
			m.report.Converted["auth_paused_watches"]++
		}
		if r.text("status") == "not_found" {
			v.State = "missing"
		}
		switch r.text("target_type") {
		case "thread":
			v.Kind, v.TID = "tid", r.number("target_id")
			opt := m.one("thread_watch_options", "watch_id", r.text("id"))
			v.HistoryConcurrency = 1
			if opt.yes("history_parallel_enabled") {
				v.HistoryConcurrency = int(opt.number("history_parallelism"))
				if v.HistoryConcurrency < 1 || v.HistoryConcurrency > 16 {
					return sourceError(opt.table, "history_parallelism", "invalid history concurrency")
				}
			}
			switch opt.text("history_mode") {
			case "full":
			case "incremental":
				v.InitMode = "from_now"
			default:
				return sourceError(opt.table, "history_mode", "unsupported initialization mode")
			}
			cursor := m.one("watch_cursors", "watch_id", r.text("id"))
			v.CursorFloor = cursor.number("last_floor")
			if v.CursorFloor < -1 || (v.BaselineComplete && v.CursorFloor < 0) {
				return sourceError(cursor.table, "last_floor", "invalid committed floor")
			}
			if v.BaselineComplete {
				v.HistoryFloor = v.CursorFloor
				cutoff := m.report.SnapshotAt
				v.HistoryBefore = &cutoff
				m.report.Converted["tid_historical_comment_cutoffs"]++
			}
			var title []string
			if err := m.db.Raw("SELECT json_extract(data,'$.title') FROM migration_source WHERE table_name='threads' AND json_extract(data,'$.tid')=?", v.TID).Scan(&title).Error; err != nil {
				return err
			}
			if len(title) > 0 {
				v.Title = title[0]
			}
		case "user":
			v.Kind, v.UID = "uid", r.number("target_id")
			cursor := m.one("user_watch_cursors", "watch_id", r.text("id"))
			v.TopicCursor = repository.UserCursor{Timestamp: cursor.number("newest_topic_at_unix"), ID: cursor.number("newest_topic_tid")}
			v.ReplyCursor = repository.UserCursor{Timestamp: cursor.number("newest_reply_at_unix"), ID: cursor.number("newest_reply_pid")}
			if v.TopicCursor.Timestamp < 0 || v.ReplyCursor.Timestamp < 0 || v.TopicCursor.ID < 0 || v.ReplyCursor.ID < 0 {
				return sourceError(cursor.table, "watermark", "negative UID watermark")
			}
		default:
			return sourceError(r.table, "target_type", "unsupported watch type")
		}
		var rules, periods []oldWindow
		r.decode("schedule_json", &rules)
		r.decode("no_fetch_periods_json", &periods)
		for _, rule := range rules {
			if rule.Interval < 30 || rule.Interval > 86400 {
				return sourceError(r.table, "schedule_json", "invalid rule interval")
			}
			w := window(r, rule)
			// Rust 仅对频率规则把 00:00–23:59 视为整天，免拉取规则不做此转换。
			if w.Start == w.End || (w.Start == "00:00" && w.End == "23:59") {
				w.Start, w.End = "00:00", "00:00"
			}
			// Rust 跨午夜频率规则还覆盖开始日凌晨；拆分后保持其实际匹配范围和优先级。
			if w.End != "00:00" && w.End < w.Start {
				late := w
				late.End = "00:00"
				v.IntervalRules = append(v.IntervalRules, repository.IntervalRule{TimeWindow: late, IntervalSeconds: rule.Interval})
				days := append([]int(nil), w.Weekdays...)
				for _, d := range w.Weekdays {
					days = append(days, d%7+1)
				}
				sort.Ints(days)
				w.Weekdays = slices.Compact(days)
				w.Start = "00:00"
			}
			v.IntervalRules = append(v.IntervalRules, repository.IntervalRule{TimeWindow: w, IntervalSeconds: rule.Interval})
		}
		for _, period := range periods {
			v.NoFetchPeriods = append(v.NoFetchPeriods, window(r, period))
		}
		if err := m.related("watch_notification_channels", "watch_id", r.text("id"), func(link row) error {
			v.ChannelIDs = append(v.ChannelIDs, m.id("notification_channels", link.text("channel_id")))
			return nil
		}); err != nil {
			return err
		}
		if err := m.related("watch_notification_authors", "watch_id", r.text("id"), func(link row) error { v.AuthorUIDs = append(v.AuthorUIDs, link.number("author_uid")); return nil }); err != nil {
			return err
		}
		slices.Sort(v.ChannelIDs)
		slices.Sort(v.AuthorUIDs)
		return m.write(&v)
	}); err != nil {
		return err
	}
	// 保留已删除监控的历史 ID，后续新建监控不能复用它们。
	if err := m.exec("DELETE FROM sqlite_sequence WHERE name='watches'"); err != nil {
		return err
	}
	return m.exec("INSERT INTO sqlite_sequence(name,seq) VALUES('watches',?)", len(m.ids["watch_targets"]))
}

func (m *importer) history() error {
	if err := m.walk("crawl_runs", func(r row) error {
		watch := m.one("watch_targets", "id", r.text("watch_id"))
		v := repository.Run{ID: m.id(r.table, r.text("id")), WatchID: m.id("watch_targets", r.text("watch_id")), Source: "unknown", Status: r.text("status"), Silent: r.yes("baseline"), StartedAt: r.at("started_at"), FinishedAt: r.timestamp("completed_at"), Pages: int(r.number("pages_requested")), Saved: r.number("posts_inserted"), Error: historicalError(r, "error_kind"), TraceID: "migration:" + r.text("id")}
		if watch.text("target_type") == "thread" {
			v.TID = watch.number("target_id")
		} else {
			v.UID = watch.number("target_id")
		}
		switch r.text("trigger_kind") {
		case "scheduled":
			v.Source = "automatic"
		case "manual":
			v.Source = "manual"
		case "unknown":
		default:
			return sourceError(r.table, "trigger_kind", "unsupported run source")
		}
		switch v.Status {
		case "succeeded":
			v.Status = "success"
		case "running":
			v.Status = "interrupted"
			v.FinishedAt = &m.report.SnapshotAt
			v.Error = "迁移中断了旧采集，请按需重新执行"
			m.report.Converted["runs_interrupted"]++
		case "skipped_busy":
		case "skipped":
			v.Status = "skipped"
			if r.text("error_kind") == "no_fetch_period" {
				v.Status = "skipped_no_fetch"
			}
		case "failed":
		default:
			return sourceError(r.table, "status", "unsupported run status")
		}
		return m.write(&v)
	}); err != nil {
		return err
	}
	if err := m.walk("uid_reply_backfill_jobs", func(r row) error {
		v := repository.Backfill{ID: m.id(r.table, r.text("id")), WatchID: m.id("watch_targets", r.text("watch_id")), UID: r.number("uid"), StartAt: time.Unix(r.number("start_at_unix"), 0).UTC(), EndAt: time.Unix(r.number("end_at_unix"), 0).UTC(), Status: r.text("status"), Pages: int(r.number("pages_requested")), Candidates: int(r.number("candidates_processed")), Saved: r.number("posts_inserted"), FinishedAt: r.timestamp("completed_at"), Error: historicalError(r, "error_kind"), TraceID: "migration:" + r.text("id")}
		if v.StartAt.After(v.EndAt) {
			return sourceError(r.table, "end_at_unix", "backfill end is before start")
		}
		switch v.Status {
		case "succeeded":
			v.Status = "success"
		case "pending", "running":
			v.Status = "interrupted"
			v.FinishedAt = &m.report.SnapshotAt
			v.Error = "迁移中断了旧回填，请按需重新提交相同范围"
			m.report.Converted["backfills_interrupted"]++
		case "failed":
		default:
			return sourceError(r.table, "status", "unsupported backfill status")
		}
		return m.write(&v)
	}); err != nil {
		return err
	}
	return m.walk("thread_floor_gaps", func(r row) error {
		v := repository.FloorGap{WatchID: m.id("watch_targets", r.text("watch_id")), Floor: r.number("floor_number"), Status: r.text("status"), FirstSeen: r.at("first_detected_at"), Deadline: r.at("expires_at"), NextAttempt: int(r.number("retry_count"))}
		if v.Floor <= 0 || v.NextAttempt < 0 || v.Deadline.Before(v.FirstSeen) {
			return sourceError(r.table, "floor_number", "invalid floor gap state")
		}
		switch v.Status {
		case "pending":
			if !v.Deadline.After(m.report.SnapshotAt) || v.NextAttempt >= 6 {
				v.Status = "expired"
				m.report.Converted["gaps_expired_at_cutover"]++
			}
		case "resolved", "expired":
		default:
			return sourceError(r.table, "status", "unsupported gap status")
		}
		v.NextAttempt = min(v.NextAttempt, 6)
		return m.write(&v)
	})
}
