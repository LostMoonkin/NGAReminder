package config

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"strconv"
	"time"
	_ "time/tzdata"

	"ngareminder/service/internal/logging"
)

type Config struct {
	ListenAddress     string `json:"listen_address"`
	DatabasePath      string `json:"database_path"`
	AssetsPath        string `json:"assets_path"`
	AdminUsername     string `json:"admin_username"`
	AdminPassword     string `json:"admin_password"`
	APIToken          string `json:"api_token"`
	EncryptionKey     string `json:"encryption_key"`
	Timezone          string `json:"timezone"`
	BackgroundEnabled bool   `json:"background_enabled"`
	CookieSecure      bool   `json:"cookie_secure"`
}

// Public 是管理页/API 的允许字段；新增凭据不会因序列化 Config 而泄露。
type Public struct {
	ListenAddress     string `json:"listen_address"`
	DatabasePath      string `json:"database_path"`
	AssetsPath        string `json:"assets_path"`
	Timezone          string `json:"timezone"`
	BackgroundEnabled bool   `json:"background_enabled"`
	CookieSecure      bool   `json:"cookie_secure"`
}

func (c Config) Public() Public {
	return Public{c.ListenAddress, c.DatabasePath, c.AssetsPath, c.Timezone, c.BackgroundEnabled, c.CookieSecure}
}

func (c Config) Secrets() []string { return []string{c.AdminPassword, c.APIToken, c.EncryptionKey} }

func Load(ctx context.Context, filename string) (cfg Config, err error) {
	_, span := logging.Start(ctx, "config.load")
	defer span.End(&err)
	cfg = Config{ListenAddress: "127.0.0.1:8080", DatabasePath: "data/nga-reminder.db", AssetsPath: "data/assets", AdminUsername: "admin", Timezone: "Asia/Shanghai", BackgroundEnabled: true}
	base := "."
	if filename != "" {
		var file *os.File
		file, err = os.Open(filename)
		if err != nil {
			return cfg, logging.Wrap(err, "读取配置文件")
		}
		defer file.Close()
		decoder := json.NewDecoder(io.LimitReader(file, 1<<20))
		decoder.DisallowUnknownFields()
		if err = decoder.Decode(&cfg); err != nil {
			return cfg, logging.Wrap(err, "解析配置文件")
		}
		if err = decoder.Decode(new(any)); !errors.Is(err, io.EOF) {
			return cfg, logging.WithStack(errors.New("配置文件必须只包含一个 JSON 对象"))
		}
		err = nil
		base = filepath.Dir(filename)
	}
	for name, target := range map[string]*string{
		"LISTEN_ADDRESS": &cfg.ListenAddress, "DATABASE_PATH": &cfg.DatabasePath, "ASSETS_PATH": &cfg.AssetsPath,
		"ADMIN_USERNAME": &cfg.AdminUsername, "ADMIN_PASSWORD": &cfg.AdminPassword, "API_TOKEN": &cfg.APIToken,
		"ENCRYPTION_KEY": &cfg.EncryptionKey, "TIMEZONE": &cfg.Timezone,
	} {
		if value, ok := os.LookupEnv("NGA_REMINDER_" + name); ok {
			*target = value
		}
	}
	for name, target := range map[string]*bool{"BACKGROUND_ENABLED": &cfg.BackgroundEnabled, "COOKIE_SECURE": &cfg.CookieSecure} {
		if value, ok := os.LookupEnv("NGA_REMINDER_" + name); ok {
			*target, err = strconv.ParseBool(value)
			if err != nil {
				return cfg, logging.WithStack(fmt.Errorf("NGA_REMINDER_%s 必须是 true 或 false", name))
			}
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
			return cfg, logging.Wrap(err, "解析数据路径")
		}
	}
	return cfg, nil
}

func (c Config) validate() error {
	_, port, err := net.SplitHostPort(c.ListenAddress)
	if err != nil {
		return logging.WithStack(errors.New("listen_address 必须采用 host:port 格式"))
	}
	portNumber, err := strconv.Atoi(port)
	if err != nil || portNumber < 1 || portNumber > 65535 {
		return logging.WithStack(errors.New("listen_address 端口必须在 1～65535 之间"))
	}
	if c.DatabasePath == "" || c.DatabasePath == ":memory:" || c.AssetsPath == "" {
		return logging.WithStack(errors.New("database_path 和 assets_path 必须是非空的本地持久化路径"))
	}
	if c.AdminUsername == "" {
		return logging.WithStack(errors.New("admin_username 不能为空"))
	}
	if len(c.AdminPassword) < 12 || len(c.AdminPassword) > 72 {
		return logging.WithStack(errors.New("admin_password 必须为 12～72 字节"))
	}
	if len(c.APIToken) < 32 {
		return logging.WithStack(errors.New("api_token 至少需要 32 字节"))
	}
	key, err := base64.StdEncoding.DecodeString(c.EncryptionKey)
	if err != nil || len(key) != 32 {
		return logging.WithStack(errors.New("encryption_key 必须是 32 个随机字节的标准 Base64 编码"))
	}
	if c.Timezone == "" {
		return logging.WithStack(errors.New("timezone 不能为空"))
	}
	if _, err = time.LoadLocation(c.Timezone); err != nil {
		return logging.Wrap(err, "timezone 无效")
	}
	return nil
}
