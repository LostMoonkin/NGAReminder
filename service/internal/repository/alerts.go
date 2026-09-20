package repository

import (
	"context"
	"time"

	"ngareminder/service/internal/logging"
)

type SystemAlert struct {
	ID         int64      `json:"id"`
	Key        string     `json:"key" gorm:"index"`
	Title      string     `json:"title"`
	Body       string     `json:"body"`
	URL        string     `json:"url"`
	CreatedAt  time.Time  `json:"created_at"`
	UpdatedAt  time.Time  `json:"updated_at"`
	ResolvedAt *time.Time `json:"resolved_at"`
	Deliveries []Delivery `json:"deliveries" gorm:"-"`
}

// 与账号状态处于同一个业务事务；一次失效周期只创建一条告警。
func (s *Store) EnsureAuthAlert(ctx context.Context) (err error) {
	ctx, span := logging.Start(ctx, "repository.ensure_auth_alert")
	defer span.End(&err)
	var active []SystemAlert
	if err = s.db.WithContext(ctx).Where("key = ? AND resolved_at IS NULL", "nga_credentials_invalid").Find(&active).Error; err != nil {
		return logging.Wrap(err, "read active authentication alert")
	}
	if len(active) == 0 {
		item := SystemAlert{Key: "nga_credentials_invalid", Title: "NGA Reminder · Cookie 已失效", Body: "NGA Cookie 已失效，请登录管理台更新 Cookie 并测试连接。", URL: "/admin"}
		if err = s.db.WithContext(ctx).Create(&item).Error; err != nil {
			return logging.Wrap(err, "create authentication alert")
		}
	}
	return s.EnqueueOpenAlerts(ctx)
}

func (s *Store) EnqueueOpenAlerts(ctx context.Context) (err error) {
	ctx, span := logging.Start(ctx, "repository.enqueue_open_alerts")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Exec(`INSERT OR IGNORE INTO deliveries (event_id, alert_id, channel_id, channel_name, status, attempts, next_attempt, updated_at)
		SELECT 0, a.id, c.id, c.name, 'pending', 0, ?, ? FROM system_alerts a JOIN channels c ON c.enabled = 1 WHERE a.resolved_at IS NULL`, time.Now().UTC(), time.Now().UTC()).Error, "enqueue active system alerts")
}

func (s *Store) ResolveAuthAlert(ctx context.Context) (err error) {
	ctx, span := logging.Start(ctx, "repository.resolve_auth_alert")
	defer span.End(&err)
	now := time.Now().UTC()
	if err = s.db.WithContext(ctx).Model(&SystemAlert{}).Where("key = ? AND resolved_at IS NULL", "nga_credentials_invalid").Updates(map[string]any{"resolved_at": now, "updated_at": now}).Error; err != nil {
		return logging.Wrap(err, "resolve authentication alert")
	}
	return logging.Wrap(s.db.WithContext(ctx).Model(&Delivery{}).Where("status = 'pending' AND alert_id IN (SELECT id FROM system_alerts WHERE key = ? AND resolved_at IS NOT NULL)", "nga_credentials_invalid").Updates(map[string]any{"status": "cancelled", "error": "账号已恢复，停止未完成的失效告警"}).Error, "cancel resolved alert deliveries")
}

func (s *Store) Alert(ctx context.Context, id int64) (item SystemAlert, err error) {
	ctx, span := logging.Start(ctx, "repository.alert")
	defer span.End(&err)
	err = s.db.WithContext(ctx).First(&item, id).Error
	return item, logging.Wrap(err, "read system alert")
}

func (s *Store) Alerts(ctx context.Context, page int) (items []SystemAlert, err error) {
	ctx, span := logging.Start(ctx, "repository.alerts")
	defer span.End(&err)
	items = []SystemAlert{}
	if err = s.db.WithContext(ctx).Order("id DESC").Offset((page - 1) * 50).Limit(50).Find(&items).Error; err != nil {
		return nil, logging.Wrap(err, "read system alerts")
	}
	for i := range items {
		if err = s.db.WithContext(ctx).Where("alert_id = ?", items[i].ID).Order("id").Find(&items[i].Deliveries).Error; err != nil {
			return nil, logging.Wrap(err, "read alert deliveries")
		}
	}
	return items, nil
}
