package main

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"net/url"
	"slices"
	"sort"
	"strings"

	"ngareminder/service/internal/infrastructure"
	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/repository"
)

type integration struct {
	kind, appID, server, group string
	delivery, bot              bool
}

func (m *importer) channels() error {
	m.integrations = map[string]integration{}
	if err := m.walk("platform_integrations", func(r row) error {
		v := integration{kind: r.text("platform"), delivery: r.yes("enabled") && r.yes("delivery_enabled"), bot: r.yes("enabled") && r.yes("bot_enabled")}
		var secret struct {
			Platform    string
			Credentials map[string]string
		}
		if json.Unmarshal([]byte(m.decrypt(r, "credentials_encrypted", "")), &secret) != nil || secret.Platform != v.kind {
			return sourceError(r.table, "credentials_encrypted", "invalid integration credential structure")
		}
		switch v.kind {
		case "bark":
			v.server, v.group = "https://api.day.app", "NGA Reminder"
			if s, ok := secret.Credentials["server_url"]; ok {
				v.server = s
			}
			if s, ok := secret.Credentials["group"]; ok {
				v.group = s
			}
			u, err := url.Parse(v.server)
			if err != nil || u.Host == "" || (u.Scheme != "https" && u.Scheme != "http") {
				return sourceError(r.table, "credentials_encrypted", "invalid Bark server URL")
			}
		case "feishu":
			if m.appID != "" {
				return sourceError(r.table, "id", "Go supports one Feishu application; consolidate integrations before migration")
			}
			v.appID = secret.Credentials["app_id"]
			if v.appID == "" || secret.Credentials["app_secret"] == "" {
				return sourceError(r.table, "credentials_encrypted", "empty Feishu credentials")
			}
			m.appID = v.appID
			if err := m.write(&repository.FeishuApp{ID: 1, Secret: m.seal("feishu-app-v1", map[string]string{"app_id": v.appID, "app_secret": secret.Credentials["app_secret"]})}); err != nil {
				return err
			}
		default:
			return sourceError(r.table, "platform", "unsupported integration platform; resolve it before migration")
		}
		m.integrations[r.text("id")] = v
		return nil
	}); err != nil {
		return err
	}
	return m.walk("notification_channels", func(r row) error {
		v, ok := m.integrations[r.text("integration_id")]
		if !ok {
			return sourceError(r.table, "integration_id", "missing integration")
		}
		var secret struct {
			Platform string
			Target   map[string]string
		}
		if json.Unmarshal([]byte(m.decrypt(r, "target_encrypted", "")), &secret) != nil || secret.Platform != v.kind {
			return sourceError(r.table, "target_encrypted", "invalid notification target structure")
		}
		target := infrastructure.ChannelTarget{}
		if v.kind == "bark" {
			target.ServerURL, target.Group, target.DeviceKey = v.server, v.group, secret.Target["device_key"]
			if target.DeviceKey == "" {
				return sourceError(r.table, "target_encrypted", "empty Bark device key")
			}
		} else {
			target.ReceiveID, target.ReceiveIDType = secret.Target["receive_id"], "chat_id"
			if s, ok := secret.Target["receive_id_type"]; ok {
				target.ReceiveIDType = s
			}
			if target.ReceiveID == "" || !slices.Contains([]string{"open_id", "union_id", "user_id", "email", "chat_id"}, target.ReceiveIDType) {
				return sourceError(r.table, "target_encrypted", "invalid Feishu recipient")
			}
		}
		return m.write(&repository.Channel{ID: m.id(r.table, r.text("id")), Name: r.text("label"), Kind: v.kind, Enabled: v.delivery && r.yes("enabled"), Secret: m.seal("notification-target-v1", target)})
	})
}

func hashID(s string) string { h := sha256.Sum256([]byte(s)); return hex.EncodeToString(h[:]) }

func (m *importer) bot() error {
	m.bindings = map[string]int64{}
	owners := map[string]int64{}
	groups := map[string][]string{}
	if err := m.walk("bot_bindings", func(r row) error {
		if !r.yes("enabled") {
			m.report.Converted["disabled_bot_bindings_archived"]++
			return nil
		}
		v, ok := m.integrations[r.text("integration_id")]
		if !ok || v.kind != "feishu" {
			return sourceError(r.table, "integration_id", "binding is not for the migrated Feishu application")
		}
		if r.text("role") != "owner" {
			return sourceError(r.table, "role", "Go only supports administrators; remove or explicitly re-authorize lower-privilege bindings before migration")
		}
		actor, chat := r.text("actor_id"), r.text("conversation_id")
		if actor == "" || chat == "" {
			return sourceError(r.table, "conversation_id", "unscoped bindings cannot be mapped safely; bind a private owner first")
		}
		switch r.text("conversation_type") {
		case "private":
			if owners[actor] != 0 {
				return sourceError(r.table, "actor_id", "multiple private bindings for one actor cannot be mapped safely")
			}
			id := m.id(r.table, r.text("id"))
			owners[actor] = id
			m.bindings[r.text("id")] = id
			return m.write(&repository.BotBinding{ID: id, ActorHash: hashID(actor), Address: m.seal("bot-binding-v1", map[string]string{"actor_id": actor, "chat_id": chat}), CreatedAt: r.at("created_at")})
		case "group":
			groups[actor] = append(groups[actor], chat)
		default:
			return sourceError(r.table, "conversation_type", "unsupported bot conversation type")
		}
		return nil
	}); err != nil {
		return err
	}
	allowed := []string{}
	for actor, items := range groups {
		if owners[actor] == 0 {
			return sourceError("bot_bindings", "actor_id", "group owners also need a private owner binding")
		}
		allowed = append(allowed, items...)
	}
	sort.Strings(allowed)
	allowed = slices.Compact(allowed)
	for actor := range owners {
		items := groups[actor]
		sort.Strings(items)
		items = slices.Compact(items)
		if !slices.Equal(items, allowed) {
			return sourceError("bot_bindings", "conversation_id", "per-owner group permissions differ; Go requires the same allowed groups for all administrators")
		}
	}
	settings := repository.BotSettings{ID: 1, Groups: m.seal("bot-groups-v1", allowed), GroupCount: len(allowed), Status: "disabled"}
	for _, v := range m.integrations {
		if v.bot {
			settings.Enabled = true
			settings.Status = "disconnected"
		}
	}
	if err := m.write(&settings); err != nil {
		return err
	}
	// 报告只列实际迁入的绑定；群绑定转为授权群配置，禁用绑定只在源快照中保留。
	m.ids["bot_bindings"] = m.bindings
	m.report.Converted["bot_group_bindings_to_allowlist"] = m.report.Source["bot_bindings"] - m.report.Converted["disabled_bot_bindings_archived"] - int64(len(m.bindings))
	return m.walk("bot_inbound_events", func(r row) error {
		v, ok := m.integrations[r.text("integration_id")]
		if !ok || v.kind != "feishu" {
			return sourceError(r.table, "integration_id", "unmapped bot integration")
		}
		if r.text("platform_message_id") == "" {
			return sourceError(r.table, "platform_message_id", "empty message ID")
		}
		return m.write(&repository.BotReceipt{ID: hashID(v.appID + ":" + r.text("platform_message_id")), Result: "此消息已在旧服务处理或因迁移中止，请发送新消息", CreatedAt: r.at("received_at")})
	})
}

func (m *importer) events() error {
	if err := m.walk("post_events", func(r row) error {
		postID := m.id("posts", r.text("post_id"))
		id := m.id(r.table, r.text("id"))
		var existing []repository.InboxEvent
		if err := m.db.Where("post_id=?", postID).Find(&existing).Error; err != nil {
			return logging.WithStack(err)
		}
		if len(existing) > 0 {
			m.ids[r.table][r.text("id")] = existing[0].ID
			m.report.Converted["events_merged_by_post"]++
			return m.exec("UPDATE inbox_events SET read=read AND ?, created_at=MIN(created_at,?) WHERE id=?", r.timestamp("read_at") != nil, r.at("occurred_at"), existing[0].ID)
		}
		return m.write(&repository.InboxEvent{ID: id, PostID: postID, Read: r.timestamp("read_at") != nil, CreatedAt: r.at("occurred_at")})
	}); err != nil {
		return err
	}
	if err := m.walk("post_event_watch_matches", func(r row) error {
		watch := m.one("watch_targets", "id", r.text("watch_id"))
		label := watch.text("target_name")
		if label == "" {
			label = watch.text("target_type") + ":" + string(watch.data["target_id"])
		}
		kind := "tid"
		if watch.text("target_type") == "user" {
			kind = "uid"
		}
		return m.exec("INSERT OR IGNORE INTO event_watches(event_id,watch_id,label,kind) VALUES(?,?,?,?)", m.id("post_events", r.text("post_event_id")), m.id("watch_targets", r.text("watch_id")), label, kind)
	}); err != nil {
		return err
	}
	if err := m.walk("notification_outbox", func(r row) error {
		v := repository.Delivery{ID: m.id(r.table, r.text("id")), EventID: m.id("post_events", r.text("post_event_id")), ChannelID: m.id("notification_channels", r.text("channel_id")), Status: "failed", Attempts: int(r.number("attempt_count")), NextAttempt: r.at("next_attempt_at"), UpdatedAt: r.at("created_at"), TraceID: "migration:" + r.text("id")}
		channel := m.one("notification_channels", "id", r.text("channel_id"))
		v.ChannelName = channel.text("label")
		switch r.text("status") {
		case "delivered":
			v.Status = "sent"
			if t := r.timestamp("delivered_at"); t != nil {
				v.UpdatedAt = *t
			}
		case "pending", "sending":
			v.Error = "迁移时未确认发送成功，请核对后手动重试"
			m.report.Converted["deliveries_require_manual_retry"]++
		case "failed", "dead":
			v.Error = "旧服务投递失败，详情保留在 PG 快照；可手动重试"
		default:
			return sourceError(r.table, "status", "unsupported notification status")
		}
		var existing []repository.Delivery
		if err := m.db.Where("event_id=? AND channel_id=?", v.EventID, v.ChannelID).Find(&existing).Error; err != nil {
			return logging.WithStack(err)
		}
		if len(existing) > 0 {
			old := existing[0]
			m.ids[r.table][r.text("id")] = old.ID
			m.report.Converted["deliveries_merged_by_target"]++
			if old.Status == "sent" {
				v.Status, v.Error = "sent", ""
			}
			v.Attempts = max(old.Attempts, v.Attempts)
			return m.exec("UPDATE deliveries SET status=?,error=?,attempts=? WHERE id=?", v.Status, v.Error, v.Attempts, old.ID)
		}
		return m.write(&v)
	}); err != nil {
		return err
	}
	// 已存内容建立观察关系，不重新创建历史事件；删除监控的事件来源仍然保留。
	return m.exec(`INSERT OR IGNORE INTO watch_posts(watch_id,post_id)
	 SELECT w.id,p.id FROM watches w JOIN posts p ON (w.kind='tid' AND w.tid=p.tid) OR (w.kind='uid' AND w.uid=p.author_uid)
	 UNION SELECT ew.watch_id,e.post_id FROM event_watches ew JOIN inbox_events e ON e.id=ew.event_id`)
}

// 旧错误正文只留原快照，避免把历史 HTTP 响应中的敏感信息带入 Go 用户界面。
func historicalError(r row, field string) string {
	if strings.TrimSpace(r.text(field)) == "" {
		return ""
	}
	return "旧服务记录了错误，详情保留在 PG 快照"
}

func (m *importer) alerts() error {
	if err := m.walk("system_alerts", func(r row) error {
		return m.write(&repository.SystemAlert{ID: m.id(r.table, r.text("id")), Key: r.text("alert_key"), Title: r.text("title"), Body: r.text("body"), URL: r.text("url"), CreatedAt: r.at("created_at"), UpdatedAt: r.at("updated_at"), ResolvedAt: r.timestamp("resolved_at")})
	}); err != nil {
		return err
	}
	return m.walk("system_alert_outbox", func(r row) error {
		channel := m.one("notification_channels", "id", r.text("channel_id"))
		v := repository.Delivery{ID: m.id(r.table, r.text("id")), AlertID: m.id("system_alerts", r.text("alert_id")), ChannelID: m.id("notification_channels", r.text("channel_id")), ChannelName: channel.text("label"), Attempts: int(r.number("attempt_count")), NextAttempt: r.at("next_attempt_at"), UpdatedAt: r.at("created_at"), TraceID: "migration:" + r.text("id")}
		switch r.text("status") {
		case "delivered":
			v.Status = "sent"
			if at := r.timestamp("delivered_at"); at != nil {
				v.UpdatedAt = *at
			}
		case "pending", "sending":
			v.Status, v.Error = "failed", "迁移时未确认发送成功，请核对后手动重试"
			m.report.Converted["alert_deliveries_require_manual_retry"]++
		case "failed", "dead":
			v.Status, v.Error = "failed", "旧服务告警投递失败，详情保留在 PG 快照；可手动重试"
		default:
			return sourceError(r.table, "status", "unsupported alert notification status")
		}
		return m.write(&v)
	})
}
