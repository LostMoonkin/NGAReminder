package config

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"ngareminder/service/internal/logging"
)

func TestFileEnvironmentPrecedenceAndValidation(t *testing.T) {
	dir := t.TempDir()
	filename := filepath.Join(dir, "config.json")
	body, err := json.Marshal(map[string]any{
		"admin_password": "test-password-from-file", "api_token": strings.Repeat("a", 40),
		"encryption_key": base64.StdEncoding.EncodeToString(make([]byte, 32)),
		"timezone":       "UTC", "background_enabled": true,
	})
	if err != nil {
		t.Fatal(err)
	}
	if err = os.WriteFile(filename, body, 0600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("NGA_REMINDER_TIMEZONE", "Asia/Shanghai")
	t.Setenv("NGA_REMINDER_BACKGROUND_ENABLED", "false")
	ctx := logging.New(io.Discard).WithContext(context.Background())
	cfg, err := Load(ctx, filename)
	if err != nil {
		t.Fatal(err)
	}
	if cfg.Timezone != "Asia/Shanghai" || cfg.BackgroundEnabled || cfg.DatabasePath != filepath.Join(dir, "data/nga-reminder.db") {
		t.Fatal("环境变量覆盖或相对路径解析不正确")
	}
	saved, err := os.ReadFile(filename)
	if err != nil || string(saved) != string(body) {
		t.Fatal("加载配置不应写回文件")
	}
	for _, tc := range []struct{ name, value, field string }{
		{"NGA_REMINDER_API_TOKEN", "", "api_token"},
		{"NGA_REMINDER_ENCRYPTION_KEY", "bad-private-key", "encryption_key"},
		{"NGA_REMINDER_BACKGROUND_ENABLED", "bad-private-value", "BACKGROUND_ENABLED"},
		{"NGA_REMINDER_TIMEZONE", "Not/A_Real_Zone", "timezone"},
	} {
		t.Run(tc.field, func(t *testing.T) {
			t.Setenv(tc.name, tc.value)
			_, err := Load(ctx, filename)
			if err == nil || !strings.Contains(err.Error(), tc.field) {
				t.Fatalf("应指出无效字段 %s，得到 %v", tc.field, err)
			}
			if strings.Contains(err.Error(), "bad-private") {
				t.Fatal("错误不应包含无效配置的秘密值")
			}
		})
	}
}
