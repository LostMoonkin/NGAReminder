package logging

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/rs/zerolog"
)

func TestConsoleLogsPreserveFieldsAndRedaction(t *testing.T) {
	var output bytes.Buffer
	log := NewConsole(&output)
	log.SetSecrets("fake-secret", ":")
	ctx, span := Start(log.WithContext(context.Background()), "test.console")
	zerolog.Ctx(ctx).Info().Str("event", "server_started").Str("listen_address", "127.0.0.1:8989").Msg("HTTP server started")
	err := WithStack(errors.New("credential fake-secret: rejected"))
	Error(ctx, err, "request failed", zerolog.ErrorLevel)
	span.End(&err)

	text := output.String()
	if strings.Contains(text, "fake-secret") || strings.Contains(text, "\x1b[") {
		t.Fatalf("console output contains a credential or ANSI escape: %s", text)
	}
	lines := strings.Split(strings.TrimSpace(text), "\n")
	if len(lines) != 4 {
		t.Fatalf("expected four text events, got %d: %s", len(lines), text)
	}
	for _, line := range lines {
		if json.Valid([]byte(line)) || len(line) < 26 {
			t.Fatalf("expected a timestamped text event: %s", line)
		}
		if _, err := time.Parse("2006-01-02 15:04:05 -07:00", line[:26]); err != nil {
			t.Fatalf("console timestamp is missing its date, time, or offset: %s", line)
		}
		for _, field := range []string{"trace_id=" + span.TraceID, "span_id=" + span.ID, "parent_span_id=", "operation=test.console"} {
			if !strings.Contains(line, field) {
				t.Fatalf("console event lost %q: %s", field, line)
			}
		}
	}
	for _, value := range []string{"INF HTTP server started listen_address=127.0.0.1:8989", "event=server_started", "ERR request failed", `error="credential [REDACTED][REDACTED] rejected"`, "causes=", "stack=", "TestConsoleLogsPreserveFieldsAndRedaction", "logging_test.go", "line", "event=end", "result=error", "duration_ms="} {
		if !strings.Contains(text, value) {
			t.Fatalf("console output lost %q: %s", value, text)
		}
	}
}

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
