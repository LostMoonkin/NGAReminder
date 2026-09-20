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
		"api_token":      "x",
		"encryption_key": base64.StdEncoding.EncodeToString(make([]byte, 32)),
		"timezone":       "UTC", "background_enabled": true,
		"store_raw_payload": false, "max_download_bytes": 1234,
		"listen_address": "127.0.0.1:0", "unused_setting": true,
	})
	if err != nil {
		t.Fatal(err)
	}
	if err = os.WriteFile(filename, body, 0600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("NGA_REMINDER_TIMEZONE", "Asia/Shanghai")
	t.Setenv("NGA_REMINDER_BACKGROUND_ENABLED", "false")
	t.Setenv("NGA_REMINDER_STORE_RAW_PAYLOAD", "true")
	t.Setenv("NGA_REMINDER_MAX_DOWNLOAD_BYTES", "2048")
	ctx := logging.New(io.Discard).WithContext(context.Background())
	cfg, err := Load(ctx, filename)
	if err != nil {
		t.Fatal(err)
	}
	if !cfg.StoreRawPayload || cfg.MaxDownloadBytes != 2048 {
		t.Fatal("raw/asset configuration precedence failed")
	}
	if cfg.Timezone != "Asia/Shanghai" || cfg.BackgroundEnabled || cfg.DatabasePath != filepath.Join(dir, "data/nga-reminder.db") {
		t.Fatal("incorrect environment override or relative path resolution")
	}
	if cfg.APIToken != "x" || cfg.ListenAddress != "127.0.0.1:0" {
		t.Fatal("short tokens and dynamic ports supported by the old service must be preserved")
	}
	saved, err := os.ReadFile(filename)
	if err != nil || string(saved) != string(body) {
		t.Fatal("loading configuration must not modify the file")
	}
	for _, tc := range []struct{ name, value, field string }{
		{"NGA_REMINDER_API_TOKEN", "", "api_token"},
		{"NGA_REMINDER_API_TOKEN", " \t\n", "api_token"},
		{"NGA_REMINDER_ENCRYPTION_KEY", "bad-private-key", "encryption_key"},
		{"NGA_REMINDER_BACKGROUND_ENABLED", "bad-private-value", "BACKGROUND_ENABLED"},
		{"NGA_REMINDER_TIMEZONE", "Not/A_Real_Zone", "timezone"},
		{"NGA_REMINDER_MAX_DOWNLOAD_BYTES", "0", "max_download_bytes"},
		{"NGA_REMINDER_MAX_DOWNLOAD_BYTES", "invalid", "MAX_DOWNLOAD_BYTES"},
		{"NGA_REMINDER_STORE_RAW_PAYLOAD", "invalid", "STORE_RAW_PAYLOAD"},
	} {
		t.Run(tc.field, func(t *testing.T) {
			t.Setenv(tc.name, tc.value)
			_, err := Load(ctx, filename)
			if err == nil || !strings.Contains(err.Error(), tc.field) {
				t.Fatalf("expected invalid field %s in error, got %v", tc.field, err)
			}
			if strings.Contains(err.Error(), "bad-private") {
				t.Fatal("errors must not contain secret values from invalid configuration")
			}
		})
	}
}
