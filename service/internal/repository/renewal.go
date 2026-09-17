package repository

import (
	"context"
	"errors"
	"gorm.io/gorm"
	"ngareminder/service/internal/logging"
	"time"
)

type RenewalSettings struct {
	ID         int    `json:"-"`
	Enabled    bool   `json:"enabled"`
	BindingID  int64  `json:"binding_id"`
	Secret     []byte `json:"-"`
	Configured bool   `json:"configured" gorm:"-"`
}
type RenewalRequest struct {
	ID        string    `json:"id" gorm:"primaryKey"`
	BindingID int64     `json:"binding_id"`
	Status    string    `json:"status"`
	Error     string    `json:"error"`
	CreatedAt time.Time `json:"created_at"`
	ExpiresAt time.Time `json:"expires_at"`
	Expected  []byte    `json:"-"`
}

func (s *Store) RenewalSettings(ctx context.Context) (v RenewalSettings, err error) {
	ctx, span := logging.Start(ctx, "repository.renewal_settings")
	defer span.End(&err)
	err = s.db.WithContext(ctx).First(&v, 1).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return RenewalSettings{ID: 1}, nil
	}
	v.Configured = len(v.Secret) > 0
	return v, logging.Wrap(err, "read renewal settings")
}
func (s *Store) SaveRenewalSettings(ctx context.Context, v *RenewalSettings) (err error) {
	ctx, span := logging.Start(ctx, "repository.save_renewal_settings")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Save(v).Error, "save renewal settings")
}
func (s *Store) LatestRenewal(ctx context.Context) (v RenewalRequest, err error) {
	ctx, span := logging.Start(ctx, "repository.latest_renewal")
	defer span.End(&err)
	err = s.db.WithContext(ctx).Order("created_at DESC").First(&v).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return v, nil
	}
	return v, logging.Wrap(err, "read latest renewal")
}
func (s *Store) SaveRenewal(ctx context.Context, v *RenewalRequest) (err error) {
	ctx, span := logging.Start(ctx, "repository.save_renewal")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Save(v).Error, "save renewal request")
}
func (s *Store) InterruptRenewals(ctx context.Context) (err error) {
	ctx, span := logging.Start(ctx, "repository.interrupt_renewals")
	defer span.End(&err)
	return logging.Wrap(s.db.WithContext(ctx).Model(&RenewalRequest{}).Where("status IN ?", []string{"awaiting_confirmation", "preparing", "awaiting_captcha", "submitting", "validating_cookie"}).Updates(map[string]any{"status": "interrupted", "error": "服务已重启，请重新发起续期", "expected": nil}).Error, "interrupt old renewal requests")
}
