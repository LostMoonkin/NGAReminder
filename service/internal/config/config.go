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

type Config struct {
	ListenAddress     string `json:"listen_address"`
	DatabasePath      string `json:"database_path"`
	AssetsPath        string `json:"assets_path"`
	APIToken          string `json:"api_token"`
	EncryptionKey     string `json:"encryption_key"`
	Timezone          string `json:"timezone"`
	BackgroundEnabled bool   `json:"background_enabled"`
}

// Public 是管理页/API 的允许字段；新增凭据不会因序列化 Config 而泄露。
type Public struct {
	ListenAddress     string `json:"listen_address"`
	DatabasePath      string `json:"database_path"`
	AssetsPath        string `json:"assets_path"`
	Timezone          string `json:"timezone"`
	BackgroundEnabled bool   `json:"background_enabled"`
}

func (c Config) Public() Public {
	return Public{c.ListenAddress, c.DatabasePath, c.AssetsPath, c.Timezone, c.BackgroundEnabled}
}

func (c Config) Secrets() []string { return []string{c.APIToken, c.EncryptionKey} }

func Load(ctx context.Context, filename string) (cfg Config, err error) {
	_, span := logging.Start(ctx, "config.load")
	defer span.End(&err)
	cfg = Config{ListenAddress: "0.0.0.0:8989", DatabasePath: "data/nga-reminder.db", AssetsPath: "data/assets", Timezone: "Asia/Shanghai", BackgroundEnabled: true}
	base := "."
	if filename != "" {
		var file *os.File
		file, err = os.Open(filename)
		if err != nil {
			return cfg, logging.Wrap(err, "读取配置文件")
		}
		defer file.Close()
		decoder := json.NewDecoder(file)
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
		"API_TOKEN":      &cfg.APIToken,
		"ENCRYPTION_KEY": &cfg.EncryptionKey, "TIMEZONE": &cfg.Timezone,
	} {
		if value, ok := os.LookupEnv("NGA_REMINDER_" + name); ok {
			*target = value
		}
	}
	if value, ok := os.LookupEnv("NGA_REMINDER_BACKGROUND_ENABLED"); ok {
		cfg.BackgroundEnabled, err = strconv.ParseBool(value)
		if err != nil {
			return cfg, logging.WithStack(errors.New("NGA_REMINDER_BACKGROUND_ENABLED 必须是 true 或 false"))
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
	if err != nil || portNumber < 0 || portNumber > 65535 {
		return logging.WithStack(errors.New("listen_address 端口必须在 0～65535 之间"))
	}
	if c.DatabasePath == "" || c.DatabasePath == ":memory:" || c.AssetsPath == "" {
		return logging.WithStack(errors.New("database_path 和 assets_path 必须是非空的本地持久化路径"))
	}
	if strings.TrimSpace(c.APIToken) == "" {
		return logging.WithStack(errors.New("api_token 不能为空或全为空白"))
	}
	key, err := base64.StdEncoding.DecodeString(c.EncryptionKey)
	if err != nil || len(key) != 32 {
		return logging.WithStack(errors.New("encryption_key 必须是标准 Base64 编码，解码后为 32 字节"))
	}
	if c.Timezone == "" {
		return logging.WithStack(errors.New("timezone 不能为空"))
	}
	if _, err = time.LoadLocation(c.Timezone); err != nil {
		return logging.Wrap(err, "timezone 无效")
	}
	return nil
}
