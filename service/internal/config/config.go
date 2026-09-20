package config

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"io"
	"net"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"
	_ "time/tzdata"

	"ngareminder/service/internal/logging"
)

const DefaultMaxDownloadBytes int64 = 10 * 1024 * 1024

type Config struct {
	MaxDownloadBytes  int64  `json:"max_download_bytes"`
	StoreRawPayload   bool   `json:"store_raw_payload"`
	ListenAddress     string `json:"listen_address"`
	DatabasePath      string `json:"database_path"`
	AssetsPath        string `json:"assets_path"`
	APIToken          string `json:"api_token"`
	EncryptionKey     string `json:"encryption_key"`
	Timezone          string `json:"timezone"`
	NGAUserAgent      string `json:"nga_user_agent"`
	BackgroundEnabled bool   `json:"background_enabled"`
}

// Public 是管理页/API 的允许字段；新增凭据不会因序列化 Config 而泄露。
type Public struct {
	MaxDownloadBytes  int64  `json:"max_download_bytes"`
	StoreRawPayload   bool   `json:"store_raw_payload"`
	ListenAddress     string `json:"listen_address"`
	DatabasePath      string `json:"database_path"`
	AssetsPath        string `json:"assets_path"`
	Timezone          string `json:"timezone"`
	BackgroundEnabled bool   `json:"background_enabled"`
}

func (c Config) Public() Public {
	return Public{ListenAddress: c.ListenAddress, DatabasePath: c.DatabasePath, AssetsPath: c.AssetsPath, Timezone: c.Timezone, BackgroundEnabled: c.BackgroundEnabled, MaxDownloadBytes: c.MaxDownloadBytes, StoreRawPayload: c.StoreRawPayload}
}

func (c Config) Secrets() []string { return []string{c.APIToken, c.EncryptionKey} }

func Load(ctx context.Context, filename string) (cfg Config, err error) {
	_, span := logging.Start(ctx, "config.load")
	defer span.End(&err)
	cfg = Config{MaxDownloadBytes: DefaultMaxDownloadBytes, ListenAddress: "0.0.0.0:8989", DatabasePath: "data/nga-reminder.db", AssetsPath: "data/assets", Timezone: "Asia/Shanghai", NGAUserAgent: "Mozilla/5.0 (compatible; NGA-Reminder/0.1)", BackgroundEnabled: true}
	base := "."
	if filename != "" {
		var file *os.File
		file, err = os.Open(filename)
		if err != nil {
			return cfg, logging.Wrap(err, "read configuration file")
		}
		defer file.Close()
		decoder := json.NewDecoder(file)
		if err = decoder.Decode(&cfg); err != nil {
			return cfg, logging.Wrap(err, "decode configuration file")
		}
		if err = decoder.Decode(new(any)); !errors.Is(err, io.EOF) {
			return cfg, logging.WithStack(errors.New("configuration file must contain exactly one JSON object"))
		}
		err = nil
		base = filepath.Dir(filename)
	}
	for name, target := range map[string]*string{
		"LISTEN_ADDRESS": &cfg.ListenAddress, "DATABASE_PATH": &cfg.DatabasePath, "ASSETS_PATH": &cfg.AssetsPath,
		"API_TOKEN":      &cfg.APIToken,
		"ENCRYPTION_KEY": &cfg.EncryptionKey, "TIMEZONE": &cfg.Timezone,
		"NGA_USER_AGENT": &cfg.NGAUserAgent,
	} {
		if value, ok := os.LookupEnv("NGA_REMINDER_" + name); ok {
			*target = value
		}
	}
	if value, ok := os.LookupEnv("NGA_REMINDER_BACKGROUND_ENABLED"); ok {
		cfg.BackgroundEnabled, err = strconv.ParseBool(value)
		if err != nil {
			return cfg, logging.WithStack(errors.New("NGA_REMINDER_BACKGROUND_ENABLED must be true or false"))
		}
	}
	if value, ok := os.LookupEnv("NGA_REMINDER_STORE_RAW_PAYLOAD"); ok {
		cfg.StoreRawPayload, err = strconv.ParseBool(value)
		if err != nil {
			return cfg, logging.WithStack(errors.New("NGA_REMINDER_STORE_RAW_PAYLOAD must be true or false"))
		}
	}
	if value, ok := os.LookupEnv("NGA_REMINDER_MAX_DOWNLOAD_BYTES"); ok {
		cfg.MaxDownloadBytes, err = strconv.ParseInt(value, 10, 64)
		if err != nil {
			return cfg, logging.WithStack(errors.New("NGA_REMINDER_MAX_DOWNLOAD_BYTES must be an integer"))
		}
	}
	if err = cfg.validate(); err != nil {
		return cfg, err
	}
	for _, path := range []*string{&cfg.DatabasePath, &cfg.AssetsPath} {
		if !filepath.IsAbs(*path) {
			*path = filepath.Join(base, *path)
		}
		*path, err = filepath.Abs(*path)
		if err != nil {
			return cfg, logging.Wrap(err, "resolve data paths")
		}
	}
	return cfg, nil
}

func (c Config) validate() error {
	if c.MaxDownloadBytes <= 0 || c.MaxDownloadBytes == int64(^uint64(0)>>1) {
		return logging.WithStack(errors.New("max_download_bytes must be positive and allow a one-byte overflow check"))
	}
	_, port, err := net.SplitHostPort(c.ListenAddress)
	if err != nil {
		return logging.WithStack(errors.New("listen_address must use the host:port format"))
	}
	portNumber, err := strconv.Atoi(port)
	if err != nil || portNumber < 0 || portNumber > 65535 {
		return logging.WithStack(errors.New("listen_address port must be between 0 and 65535"))
	}
	if c.DatabasePath == "" || c.DatabasePath == ":memory:" || c.AssetsPath == "" {
		return logging.WithStack(errors.New("database_path and assets_path must be non-empty local persistent paths"))
	}
	if strings.TrimSpace(c.APIToken) == "" {
		return logging.WithStack(errors.New("api_token must not be empty or contain only whitespace"))
	}
	key, err := base64.StdEncoding.DecodeString(c.EncryptionKey)
	if err != nil || len(key) != 32 {
		return logging.WithStack(errors.New("encryption_key must be standard Base64 encoding of 32 bytes"))
	}
	if c.Timezone == "" {
		return logging.WithStack(errors.New("timezone must not be empty"))
	}
	if _, err = time.LoadLocation(c.Timezone); err != nil {
		return logging.Wrap(err, "invalid timezone")
	}
	return nil
}
