package logging

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"strings"
	"testing"

	"github.com/rs/zerolog"
)

func TestShortSecretsPreserveLogStructure(t *testing.T) {
	for _, secret := range []string{":", "1", `"`} {
		t.Run(secret, func(t *testing.T) {
			var output bytes.Buffer
			log := New(&output)
			log.SetSecrets(secret)
			l := log.With().Str("trace_id", "trace:1").Int("status", 201).Logger()
			Error(l.WithContext(context.Background()), WithStack(errors.New("credential "+secret)), "request failed", zerolog.ErrorLevel)
			var event map[string]any
			if err := json.Unmarshal(output.Bytes(), &event); err != nil {
				t.Fatalf("短 token 不应破坏 JSON：%v", err)
			}
			if event["trace_id"] != "trace:1" || event["status"] != float64(201) || event["level"] != "error" {
				t.Fatal("脱敏修改了关联字段、数字或级别")
			}
			if event["error"] != "credential [REDACTED]" || !strings.Contains(event["causes"].([]any)[0].(string), "[REDACTED]") {
				t.Fatal("错误中的凭据没有脱敏")
			}
		})
	}
}
