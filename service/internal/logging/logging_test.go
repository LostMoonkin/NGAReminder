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
				t.Fatalf("short tokens must not corrupt JSON: %v", err)
			}
			if event["trace_id"] != "trace:1" || event["status"] != float64(201) || event["level"] != "error" {
				t.Fatal("redaction modified trace fields, numbers, or log level")
			}
			if event["error"] != "credential [REDACTED]" || !strings.Contains(event["causes"].([]any)[0].(string), "[REDACTED]") {
				t.Fatal("credentials in errors were not redacted")
			}
		})
	}
}
