package repository

import (
	"bytes"
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"gorm.io/gorm"
	"ngareminder/service/internal/logging"
)

func TestSQLFilesPreserveErrorsAndOmitParameters(t *testing.T) {
	var stdout bytes.Buffer
	log := logging.NewConsole(&stdout)
	dir := t.TempDir()
	if err := log.EnableFiles(filepath.Join(dir, "logs"), "UTC"); err != nil {
		t.Fatal(err)
	}
	defer log.Close()
	ctx, span := logging.Start(log.WithContext(context.Background()), "test.sql_files")
	defer span.End(nil)
	store, err := Open(ctx, filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close(ctx)
	db := store.db.WithContext(ctx)
	if err := db.Exec("CREATE TABLE logging_probe (id INTEGER PRIMARY KEY, value TEXT)").Error; err != nil {
		t.Fatal(err)
	}
	if err := db.Exec("INSERT INTO logging_probe VALUES (?, ?)", 1, "must-never-be-logged").Error; err != nil {
		t.Fatal(err)
	}
	var absent struct{ ID int }
	if err := db.Table("logging_probe").Where("id = ?", 2).First(&absent).Error; !errors.Is(err, gorm.ErrRecordNotFound) {
		t.Fatal("expected missing record")
	}
	if strings.Contains(stdout.String(), "log_type=sql") {
		t.Fatal("successful or missing-record SQL leaked to stdout")
	}
	if err := db.Exec("INSERT INTO logging_probe VALUES (?, ?)", 1, "must-never-be-logged").Error; err == nil {
		t.Fatal("expected unique constraint failure")
	}
	date := time.Now().UTC().Format(time.DateOnly)
	raw, err := os.ReadFile(filepath.Join(dir, "logs", "sql-"+date+".log"))
	if err != nil {
		t.Fatal(err)
	}
	for _, text := range []string{string(raw), stdout.String()} {
		if strings.Contains(text, "must-never-be-logged") {
			t.Fatal("SQL bound parameter was logged")
		}
		for _, value := range []string{"ERR SQL query failed", "trace_id=" + span.TraceID, "causes=", "stack=", "duration_ms=", "rows="} {
			if !strings.Contains(text, value) {
				t.Fatalf("SQL error lost %s", value)
			}
		}
	}
	if !strings.Contains(string(raw), "VALUES (?, ?)") || !strings.Contains(string(raw), "INF") {
		t.Fatal("SQL file omitted successful parameterized query")
	}
	before := len(raw)
	if err := store.db.WithContext(logging.Quiet(ctx)).Exec("SELECT 1").Error; err != nil {
		t.Fatal(err)
	}
	quiet, err := os.ReadFile(filepath.Join(dir, "logs", "sql-"+date+".log"))
	if err != nil || len(quiet) != before {
		t.Fatal("quiet background query wrote SQL logs")
	}
}
