package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/rs/zerolog"
	"ngareminder/service/internal/logging"
)

type sourceTable struct {
	Name    string   `json:"name"`
	Columns []string `json:"columns"`
	Types   []string `json:"types"`
}
type sourceHeader struct {
	Format        string        `json:"format"`
	At            time.Time     `json:"at"`
	Schema        string        `json:"schema"`
	ServerVersion string        `json:"server_version"`
	Tables        []sourceTable `json:"tables"`
}
type sourceRecord struct {
	Header   *sourceHeader    `json:"header,omitempty"`
	Table    string           `json:"table,omitempty"`
	Row      json.RawMessage  `json:"row,omitempty"`
	Complete bool             `json:"complete,omitempty"`
	Counts   map[string]int64 `json:"counts,omitempty"`
}

func exportPG(ctx context.Context, schema, destination string) (err error) {
	ctx, span := logging.Start(ctx, "migration.export_postgres")
	defer span.End(&err)
	if err = unused(destination); err != nil {
		return err
	}
	dsn := os.Getenv("NGA_MIGRATE_PG_URL")
	if dsn == "" {
		return logging.WithStack(errors.New("NGA_MIGRATE_PG_URL is required when -source is omitted"))
	}
	cfg, err := pgx.ParseConfig(dsn)
	if err != nil {
		return logging.Wrap(err, "parse PostgreSQL connection")
	}
	cfg.RuntimeParams["default_transaction_read_only"] = "on"
	cfg.RuntimeParams["timezone"] = "UTC"
	cfg.RuntimeParams["bytea_output"] = "hex"
	cfg.RuntimeParams["row_security"] = "off"
	cfg.ConnectTimeout = 15 * time.Second
	conn, err := pgx.ConnectConfig(ctx, cfg)
	if err != nil {
		return logging.Wrap(err, "connect to source PostgreSQL")
	}
	defer conn.Close(context.Background())
	tx, err := conn.BeginTx(ctx, pgx.TxOptions{IsoLevel: pgx.RepeatableRead, AccessMode: pgx.ReadOnly})
	if err != nil {
		return logging.Wrap(err, "start read-only PostgreSQL snapshot")
	}
	defer tx.Rollback(context.Background())
	h := sourceHeader{Format: "nga-rust-pg-v1", Schema: schema}
	if err = tx.QueryRow(ctx, "SELECT CURRENT_TIMESTAMP, current_setting('server_version')").Scan(&h.At, &h.ServerVersion); err != nil {
		return logging.WithStack(err)
	}
	// pg_catalog 不会因缺少 SELECT 权限而隐藏整张表；后续读取会明确报错，避免导出不完整。
	rows, err := tx.Query(ctx, `SELECT c.relname,a.attname,pg_catalog.format_type(a.atttypid,a.atttypmod)
	 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
	 JOIN pg_catalog.pg_attribute a ON a.attrelid=c.oid
	 WHERE n.nspname=$1 AND c.relkind IN ('r','p') AND a.attnum>0 AND NOT a.attisdropped
	 ORDER BY c.relname,a.attnum`, schema)
	if err != nil {
		return logging.Wrap(err, "read source schema")
	}
	for rows.Next() {
		var table, column, kind string
		if err = rows.Scan(&table, &column, &kind); err != nil {
			rows.Close()
			return logging.WithStack(err)
		}
		if len(h.Tables) == 0 || h.Tables[len(h.Tables)-1].Name != table {
			h.Tables = append(h.Tables, sourceTable{Name: table})
		}
		t := &h.Tables[len(h.Tables)-1]
		t.Columns, t.Types = append(t.Columns, column), append(t.Types, kind)
	}
	if err = rows.Err(); err != nil {
		return logging.WithStack(err)
	}
	rows.Close()
	if len(h.Tables) == 0 {
		return logging.WithStack(errors.New("source schema has no visible tables"))
	}
	file, err := os.CreateTemp(filepath.Dir(destination), ".pg-export-*")
	if err != nil {
		return logging.Wrap(err, "create snapshot file")
	}
	defer os.Remove(file.Name())
	defer file.Close()
	enc := json.NewEncoder(file)
	if err = enc.Encode(sourceRecord{Header: &h}); err != nil {
		return logging.WithStack(err)
	}
	counts := map[string]int64{}
	for _, table := range h.Tables {
		counts[table.Name] = 0
		// row_to_json 保留 NULL、64 位整数及 BYTEA 原值，不经过浮点中转。
		query := "SELECT row_to_json(t)::text FROM " + pgx.Identifier{schema, table.Name}.Sanitize() + " t"
		rows, err = tx.Query(ctx, query)
		if err != nil {
			return logging.Wrap(err, "export source table "+table.Name)
		}
		for rows.Next() {
			var raw string
			if err = rows.Scan(&raw); err != nil {
				rows.Close()
				return logging.WithStack(err)
			}
			if err = enc.Encode(sourceRecord{Table: table.Name, Row: json.RawMessage(raw)}); err != nil {
				rows.Close()
				return logging.Wrap(err, "write source row")
			}
			counts[table.Name]++
		}
		if err = rows.Err(); err != nil {
			return logging.Wrap(err, "finish source table "+table.Name)
		}
		rows.Close()
		zerolog.Ctx(ctx).Info().Str("table", table.Name).Int64("rows", counts[table.Name]).Msg("Source table exported")
	}
	if err = tx.Commit(ctx); err != nil {
		return logging.Wrap(err, "finish read-only PostgreSQL snapshot")
	}
	if err = enc.Encode(sourceRecord{Complete: true, Counts: counts}); err != nil {
		return logging.WithStack(err)
	}
	if err = file.Sync(); err != nil {
		return logging.Wrap(err, "flush PostgreSQL snapshot")
	}
	if err = file.Close(); err != nil {
		return logging.WithStack(err)
	}
	if err = publish(file.Name(), destination); err != nil {
		return err
	}
	zerolog.Ctx(ctx).Info().Int("tables", len(counts)).Msg("Complete PostgreSQL snapshot published")
	return nil
}

func sourceError(table, field, reason string) error {
	return logging.WithStack(fmt.Errorf("source %s.%s: %s", table, field, reason))
}
