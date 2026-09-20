package repository

import (
	"context"
	"errors"
	"time"

	"gorm.io/gorm"
	"gorm.io/gorm/clause"
	"ngareminder/service/internal/logging"
)

type FeishuApp struct {
	ID         int    `json:"-"`
	Secret     []byte `json:"-"`
	Configured bool   `json:"configured" gorm:"-"`
}
type Channel struct {
	ID      int64  `json:"id"`
	Name    string `json:"name"`
	Kind    string `json:"kind"`
	Enabled bool   `json:"enabled"`
	Secret  []byte `json:"-"`
}
type InboxEvent struct {
	ID         int64        `json:"id"`
	PostID     int64        `json:"post_id" gorm:"uniqueIndex"`
	Read       bool         `json:"read"`
	CreatedAt  time.Time    `json:"created_at"`
	Post       Post         `json:"post" gorm:"-"`
	Sources    []EventWatch `json:"sources" gorm:"-"`
	Deliveries []Delivery   `json:"deliveries" gorm:"-"`
}
type EventWatch struct {
	Kind    string `json:"kind"`
	EventID int64  `json:"event_id" gorm:"primaryKey;autoIncrement:false"`
	WatchID int64  `json:"watch_id" gorm:"primaryKey;autoIncrement:false"`
	Label   string `json:"label"`
}
type WatchPost struct {
	WatchID int64 `gorm:"primaryKey;autoIncrement:false"`
	PostID  int64 `gorm:"primaryKey;autoIncrement:false"`
}
type Delivery struct {
	AlertID     int64     `json:"alert_id,omitempty" gorm:"default:0;uniqueIndex:delivery_target"`
	ID          int64     `json:"id"`
	EventID     int64     `json:"event_id" gorm:"default:0;uniqueIndex:delivery_target"`
	ChannelID   int64     `json:"channel_id" gorm:"default:0;uniqueIndex:delivery_target"`
	ChannelName string    `json:"channel_name"`
	Status      string    `json:"status"`
	Attempts    int       `json:"attempts"`
	NextAttempt time.Time `json:"next_attempt"`
	Error       string    `json:"error"`
	TraceID     string    `json:"trace_id"`
	UpdatedAt   time.Time `json:"updated_at"`
}

func (s *Store) FeishuApp(ctx context.Context) (app FeishuApp, err error) {
	ctx, span := logging.Start(ctx, "repository.feishu_app")
	defer span.End(&err)
	err = s.db.WithContext(ctx).First(&app, 1).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return FeishuApp{ID: 1}, nil
	}
	app.Configured = len(app.Secret) > 0
	return app, logging.Wrap(err, "read Feishu app")
}
func (s *Store) SaveFeishuApp(ctx context.Context, app *FeishuApp) (err error) {
	ctx, span := logging.Start(ctx, "repository.save_feishu_app")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Save(app).Error, "save Feishu app")
}
func (s *Store) Channels(ctx context.Context) (items []Channel, err error) {
	ctx, span := logging.Start(ctx, "repository.channels")
	defer span.End(&err)
	items = []Channel{}
	err = s.db.WithContext(ctx).Order("id").Find(&items).Error
	return items, logging.Wrap(err, "read notification channels")
}
func (s *Store) Channel(ctx context.Context, id int64) (item Channel, err error) {
	ctx, span := logging.Start(ctx, "repository.channel")
	defer span.End(&err)
	err = s.db.WithContext(ctx).First(&item, id).Error
	return item, logging.Wrap(err, "read notification channel")
}
func (s *Store) SaveChannel(ctx context.Context, item *Channel) (err error) {
	ctx, span := logging.Start(ctx, "repository.save_channel")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Save(item).Error, "save notification channel")
}
func (s *Store) DeleteChannel(ctx context.Context, id int64) (err error) {
	ctx, span := logging.Start(ctx, "repository.delete_channel")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Delete(&Channel{}, id).Error, "delete notification channel")
}
func (s *Store) PostByKey(ctx context.Context, tid int64, key string) (post Post, err error) {
	ctx, span := logging.Start(ctx, "repository.post_by_key")
	defer span.End(&err)
	err = s.db.WithContext(ctx).Where("tid = ? AND key = ?", tid, key).First(&post).Error
	return post, logging.Wrap(err, "read saved post")
}
func (s *Store) ObservePost(ctx context.Context, watchID, postID int64) (fresh bool, err error) {
	ctx, span := logging.Start(ctx, "repository.observe_post")
	defer span.End(&err)
	r := s.db.WithContext(ctx).Clauses(clause.OnConflict{DoNothing: true}).Create(&WatchPost{watchID, postID})
	return r.RowsAffected > 0, logging.Wrap(r.Error, "record watch content observation")
}
func (s *Store) MatchEvent(ctx context.Context, postID int64, watch Watch) (event InboxEvent, err error) {
	ctx, span := logging.Start(ctx, "repository.match_event")
	defer span.End(&err)
	event = InboxEvent{PostID: postID}
	if err = s.db.WithContext(ctx).Clauses(clause.OnConflict{DoNothing: true}).Create(&event).Error; err != nil {
		return event, logging.Wrap(err, "create inbox event")
	}
	if err = s.db.WithContext(ctx).Where("post_id = ?", postID).First(&event).Error; err != nil {
		return event, logging.Wrap(err, "read inbox event")
	}
	err = s.db.WithContext(ctx).Clauses(clause.OnConflict{DoNothing: true}).Create(&EventWatch{EventID: event.ID, WatchID: watch.ID, Label: watch.Label, Kind: watch.Kind}).Error
	return event, logging.Wrap(err, "record event source")
}
func (s *Store) Enqueue(ctx context.Context, eventID int64, channel Channel) (err error) {
	ctx, span := logging.Start(ctx, "repository.enqueue_notification")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Clauses(clause.OnConflict{DoNothing: true}).Create(&Delivery{EventID: eventID, ChannelID: channel.ID, ChannelName: channel.Name, Status: "pending", NextAttempt: time.Now().UTC()}).Error, "enqueue notification")
}
func (s *Store) Inbox(ctx context.Context, page int) ([]InboxEvent, error) {
	return s.FilteredInbox(ctx, page, "all")
}

// 先筛选再分页，避免“未读”只筛选当前 50 条而漏掉更早的事件。
func (s *Store) FilteredInbox(ctx context.Context, page int, filter string) (items []InboxEvent, err error) {
	ctx, span := logging.Start(ctx, "repository.inbox")
	defer span.End(&err)
	items = []InboxEvent{}
	query := s.db.WithContext(ctx)
	switch filter {
	case "unread":
		query = query.Where("read = ?", false)
	case "delivery":
		query = query.Where("EXISTS (SELECT 1 FROM deliveries d WHERE d.event_id = inbox_events.id AND d.status IN ?)", []string{"pending", "failed"})
	}
	if err = query.Order("id DESC").Offset((page - 1) * 50).Limit(50).Find(&items).Error; err != nil {
		return nil, logging.Wrap(err, "read inbox")
	}
	for i := range items {
		if err = s.eventDetails(ctx, &items[i]); err != nil {
			return nil, err
		}
	}
	return items, nil
}
func (s *Store) Event(ctx context.Context, id int64) (item InboxEvent, err error) {
	ctx, span := logging.Start(ctx, "repository.event")
	defer span.End(&err)
	if err = s.db.WithContext(ctx).First(&item, id).Error; err != nil {
		return item, logging.Wrap(err, "read event")
	}
	err = s.eventDetails(ctx, &item)
	return item, err
}
func (s *Store) eventDetails(ctx context.Context, item *InboxEvent) error {
	if err := s.db.WithContext(ctx).First(&item.Post, item.PostID).Error; err != nil {
		return logging.Wrap(err, "read event post")
	}
	if err := s.db.WithContext(ctx).Where("event_id = ?", item.ID).Find(&item.Sources).Error; err != nil {
		return logging.Wrap(err, "read event sources")
	}
	return logging.Wrap(s.db.WithContext(ctx).Where("event_id = ?", item.ID).Order("id").Find(&item.Deliveries).Error, "read event deliveries")
}
func (s *Store) MarkRead(ctx context.Context, id int64, read bool) (err error) {
	ctx, span := logging.Start(ctx, "repository.mark_inbox_read")
	defer span.End(&err)
	r := s.db.WithContext(ctx).Model(&InboxEvent{}).Where("id = ?", id).Update("read", read)
	if r.Error == nil && r.RowsAffected == 0 {
		return logging.WithStack(ErrNotFound)
	}
	return logging.Wrap(r.Error, "mark inbox read")
}
func (s *Store) PendingDeliveries(ctx context.Context) (items []Delivery, err error) {
	ctx, span := logging.Start(ctx, "repository.pending_deliveries")
	defer span.End(&err)
	err = s.db.WithContext(ctx).Where("status = ? AND attempts < ?", "pending", 5).Order("next_attempt, id").Find(&items).Error
	return items, logging.Wrap(err, "read pending deliveries")
}
func (s *Store) Delivery(ctx context.Context, id int64) (item Delivery, err error) {
	ctx, span := logging.Start(ctx, "repository.delivery")
	defer span.End(&err)
	err = s.db.WithContext(ctx).First(&item, id).Error
	return item, logging.Wrap(err, "read delivery")
}
func (s *Store) SaveDelivery(ctx context.Context, item *Delivery) (err error) {
	ctx, span := logging.Start(ctx, "repository.save_delivery")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Save(item).Error, "save delivery result")
}

func (s *Store) CancelChannelDeliveries(ctx context.Context, id int64) (err error) {
	ctx, span := logging.Start(ctx, "repository.cancel_channel_deliveries")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Model(&Delivery{}).Where("channel_id = ? AND status != ?", id, "sent").Updates(map[string]any{"status": "cancelled", "error": "通知渠道已删除"}).Error, "cancel deleted channel deliveries")
}
