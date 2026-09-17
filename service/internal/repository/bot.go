package repository

import (
	"context"
	"errors"
	"gorm.io/gorm"
	"gorm.io/gorm/clause"
	"ngareminder/service/internal/logging"
	"time"
)

type BotSettings struct {
	ID          int        `json:"-"`
	Enabled     bool       `json:"enabled"`
	Groups      []byte     `json:"-"`
	GroupCount  int        `json:"group_count"`
	CodeHash    string     `json:"-"`
	CodeExpires *time.Time `json:"code_expires"`
	Status      string     `json:"status"`
	LastError   string     `json:"last_error"`
}
type BotBinding struct {
	ID        int64     `json:"id"`
	ActorHash string    `json:"-" gorm:"uniqueIndex"`
	Address   []byte    `json:"-"`
	CreatedAt time.Time `json:"created_at"`
}
type BotReceipt struct {
	ID        string `gorm:"primaryKey"`
	Result    string
	CreatedAt time.Time
}

func (s *Store) BotSettings(ctx context.Context) (v BotSettings, err error) {
	ctx, span := logging.Start(ctx, "repository.bot_settings")
	defer span.End(&err)
	err = s.db.WithContext(ctx).First(&v, 1).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return BotSettings{ID: 1, Status: "disabled"}, nil
	}
	return v, logging.Wrap(err, "read bot settings")
}
func (s *Store) SaveBotSettings(ctx context.Context, v *BotSettings) (err error) {
	ctx, span := logging.Start(ctx, "repository.save_bot_settings")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Save(v).Error, "save bot settings")
}
func (s *Store) BotConnectionState(ctx context.Context, status, message string) (err error) {
	ctx, span := logging.Start(ctx, "repository.bot_connection_state")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Model(&BotSettings{}).Where("id = ?", 1).Updates(map[string]any{"status": status, "last_error": message}).Error, "save bot connection state")
}
func (s *Store) Bindings(ctx context.Context) (items []BotBinding, err error) {
	ctx, span := logging.Start(ctx, "repository.bot_bindings")
	defer span.End(&err)
	items = []BotBinding{}
	err = s.db.WithContext(ctx).Order("id").Find(&items).Error
	return items, logging.Wrap(err, "read bot bindings")
}
func (s *Store) Binding(ctx context.Context, id int64) (v BotBinding, err error) {
	ctx, span := logging.Start(ctx, "repository.bot_binding")
	defer span.End(&err)
	err = s.db.WithContext(ctx).First(&v, id).Error
	return v, logging.Wrap(err, "read bot binding")
}
func (s *Store) SaveBinding(ctx context.Context, v *BotBinding) (err error) {
	ctx, span := logging.Start(ctx, "repository.save_bot_binding")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Clauses(clause.OnConflict{Columns: []clause.Column{{Name: "actor_hash"}}, DoUpdates: clause.AssignmentColumns([]string{"address"})}).Create(v).Error, "save bot binding")
}
func (s *Store) RevokeBinding(ctx context.Context, id int64) (err error) {
	ctx, span := logging.Start(ctx, "repository.revoke_bot_binding")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Delete(&BotBinding{}, id).Error, "revoke bot binding")
}
func (s *Store) ClaimBotMessage(ctx context.Context, id string) (fresh bool, err error) {
	ctx, span := logging.Start(ctx, "repository.claim_bot_message")
	defer span.End(&err)
	r := s.db.WithContext(ctx).Clauses(clause.OnConflict{DoNothing: true}).Create(&BotReceipt{ID: id})
	return r.RowsAffected == 1, logging.Wrap(r.Error, "record bot message")
}
func (s *Store) FinishBotMessage(ctx context.Context, id, result string) (err error) {
	ctx, span := logging.Start(ctx, "repository.finish_bot_message")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Model(&BotReceipt{}).Where("id = ?", id).Update("result", result).Error, "save bot command result")
}
