package main

import (
	"errors"
	"fmt"
	"sort"
	"strings"

	"ngareminder/service/internal/logging"
)

func (m *importer) validate() error {
	var integrity []string
	if err := m.db.Raw("PRAGMA integrity_check").Scan(&integrity).Error; err != nil {
		return logging.WithStack(err)
	}
	if len(integrity) != 1 || integrity[0] != "ok" {
		return logging.WithStack(errors.New("SQLite integrity check failed"))
	}
	for name, query := range map[string]string{
		"event posts":           "SELECT COUNT(*) FROM inbox_events e LEFT JOIN posts p ON p.id=e.post_id WHERE p.id IS NULL",
		"event sources":         "SELECT COUNT(*) FROM event_watches w LEFT JOIN inbox_events e ON e.id=w.event_id WHERE e.id IS NULL",
		"delivery events":       "SELECT COUNT(*) FROM deliveries d LEFT JOIN inbox_events e ON e.id=d.event_id WHERE e.id IS NULL",
		"delivery channels":     "SELECT COUNT(*) FROM deliveries d LEFT JOIN channels c ON c.id=d.channel_id WHERE c.id IS NULL",
		"post observations":     "SELECT COUNT(*) FROM watch_posts w LEFT JOIN posts p ON p.id=w.post_id WHERE p.id IS NULL",
		"comment parents":       "SELECT COUNT(*) FROM posts c LEFT JOIN posts p ON p.tid=c.tid AND p.key=c.parent_key WHERE c.kind='comment' AND p.id IS NULL",
		"post resources":        "SELECT COUNT(*) FROM posts p,json_each(p.resources) j LEFT JOIN resources r ON r.url=j.value WHERE r.url IS NULL",
		"watch channels":        "SELECT COUNT(*) FROM watches w,json_each(w.channel_ids) j LEFT JOIN channels c ON c.id=j.value WHERE c.id IS NULL",
		"watch targets":         "SELECT COUNT(*) FROM (SELECT kind,tid,uid FROM watches GROUP BY kind,tid,uid HAVING COUNT(*)>1)",
		"unfinished operations": "SELECT (SELECT COUNT(*) FROM runs WHERE status='running')+(SELECT COUNT(*) FROM backfills WHERE status IN ('running','pending'))+(SELECT COUNT(*) FROM deliveries WHERE status IN ('sending','pending'))+(SELECT COUNT(*) FROM renewal_requests WHERE expected IS NOT NULL)+(SELECT COUNT(*) FROM bot_settings WHERE code_hash<>'')",
		"renewal binding":       "SELECT COUNT(*) FROM renewal_settings r LEFT JOIN bot_bindings b ON b.id=r.binding_id WHERE r.enabled=1 AND b.id IS NULL",
		"time order":            "SELECT (SELECT COUNT(*) FROM runs WHERE finished_at < started_at)+(SELECT COUNT(*) FROM backfills WHERE end_at < start_at)+(SELECT COUNT(*) FROM floor_gaps WHERE deadline < first_seen)",
	} {
		var count int64
		if err := m.db.Raw(query).Scan(&count).Error; err != nil {
			return logging.Wrap(err, "validate "+name)
		}
		if count != 0 {
			return logging.WithStack(fmt.Errorf("converted data failed %s check: %d rows", name, count))
		}
		m.report.Converted["checks_passed"]++
	}
	// Go 历史记录允许引用已删除监控，但 ID 必须来自本次快照，不能悬空或复用。
	var watchIDs []int64
	if err := m.db.Raw("SELECT watch_id FROM runs UNION SELECT watch_id FROM backfills UNION SELECT watch_id FROM floor_gaps UNION SELECT watch_id FROM event_watches UNION SELECT watch_id FROM watch_posts").Scan(&watchIDs).Error; err != nil {
		return logging.WithStack(err)
	}
	for _, id := range watchIDs {
		if id <= 0 || id > int64(len(m.ids["watch_targets"])) {
			return sourceError("watch_targets", "id", "unmapped historical watch reference")
		}
	}
	for _, table := range strings.Fields("accounts watches posts runs feishu_apps channels bot_settings bot_bindings bot_receipts renewal_settings renewal_requests resource_settings resources inbox_events event_watches deliveries watch_posts backfills floor_gaps") {
		var count int64
		if err := m.db.Table(table).Count(&count).Error; err != nil {
			return logging.WithStack(err)
		}
		m.report.Target[table] = count
	}
	for from, to := range map[string]string{"nga_accounts": "accounts", "posts": "posts", "assets": "resources", "notification_channels": "channels", "crawl_runs": "runs", "uid_reply_backfill_jobs": "backfills", "thread_floor_gaps": "floor_gaps", "nga_account_renewal_settings": "renewal_settings", "nga_login_sessions": "renewal_requests", "bot_inbound_events": "bot_receipts"} {
		if m.report.Source[from] != m.report.Target[to] {
			return sourceError(from, "count", "source and target row counts differ")
		}
	}
	if m.report.Source["watch_targets"] != m.report.Target["watches"]+m.report.Converted["deleted_watches_archived"] {
		return sourceError("watch_targets", "count", "unaccounted watches")
	}
	if m.report.Source["post_events"] != m.report.Target["inbox_events"]+m.report.Converted["events_merged_by_post"] {
		return sourceError("post_events", "count", "unaccounted events")
	}
	if m.report.Source["notification_outbox"] != m.report.Target["deliveries"]+m.report.Converted["deliveries_merged_by_target"] {
		return sourceError("notification_outbox", "count", "unaccounted deliveries")
	}
	used := map[string]bool{}
	for _, line := range strings.Split(strings.TrimSpace(requiredSchema), "\n") {
		used[strings.Fields(line)[0]] = true
	}
	for table := range m.report.Source {
		if !used[table] {
			m.report.ArchivedOnly = append(m.report.ArchivedOnly, table)
		}
	}
	sort.Strings(m.report.ArchivedOnly)
	m.report.Notes = append(m.report.Notes,
		"原线程表中的标题映射到帖子和监控；没有已存帖子的线程及论坛、覆盖率等旧字段只保留在快照。",
		"已完成基线的 TID 监控保留水位，以快照时间屏蔽旧楼层中未曾保存的历史评论；已知缺口仍可在原截止时间前补采。",
		"缺口沿用原发现时间、截止时间和尝试次数，后续重试按 Go 的时间表执行。",
		"旧暂停原因明细、逐次投递响应、连接/租约/验证码上下文与旧错误正文保留在快照，运行时不恢复。")
	return nil
}
