package main

import (
	"context"
	"crypto/aes"
	"crypto/cipher"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"net/url"
	"os"
	"path/filepath"
	"reflect"
	"slices"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/libtnb/sqlite"
	"github.com/rs/zerolog"
	"gorm.io/gorm"
	"gorm.io/gorm/logger"
	"ngareminder/service/internal/infrastructure"
	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/repository"
)

type report struct {
	Ready          bool                        `json:"ready"`
	SnapshotAt     time.Time                   `json:"snapshot_at"`
	SnapshotSHA256 string                      `json:"snapshot_sha256"`
	Source         map[string]int64            `json:"source_rows"`
	Target         map[string]int64            `json:"target_rows"`
	Converted      map[string]int64            `json:"conversions"`
	ArchivedOnly   []string                    `json:"archive_only_tables"`
	Notes          []string                    `json:"notes"`
	Runtime        map[string]string           `json:"runtime_environment"`
	IDs            map[string]map[string]int64 `json:"id_mapping"`
}
type importer struct {
	ctx          context.Context
	db           *gorm.DB
	old          cipher.AEAD
	cipher       *infrastructure.CredentialCipher
	ids          map[string]map[string]int64
	report       report
	o            options
	accountID    string
	appID        string
	integrations map[string]integration
	bindings     map[string]int64
}

// 源数据错误中仅包含表、字段和原因，不包含原值；统一在 convert 边界转为带栈错误。
type row struct {
	table string
	data  map[string]json.RawMessage
}

func (r row) text(k string) string {
	if len(r.data[k]) == 0 || string(r.data[k]) == "null" {
		return ""
	}
	var v string
	if json.Unmarshal(r.data[k], &v) != nil {
		panic(sourceError(r.table, k, "expected a string"))
	}
	return v
}
func (r row) number(k string) int64 {
	if len(r.data[k]) == 0 || string(r.data[k]) == "null" {
		return 0
	}
	var v int64
	if json.Unmarshal(r.data[k], &v) != nil {
		panic(sourceError(r.table, k, "expected a 64-bit integer"))
	}
	return v
}
func (r row) yes(k string) bool {
	if string(r.data[k]) == "true" {
		return true
	}
	if string(r.data[k]) == "false" {
		return false
	}
	v := r.number(k)
	if v != 0 && v != 1 {
		panic(sourceError(r.table, k, "expected 0 or 1"))
	}
	return v == 1
}
func (r row) timestamp(k string) *time.Time {
	v := r.text(k)
	if v == "" {
		return nil
	}
	t, err := time.Parse(time.RFC3339Nano, v)
	if err != nil {
		panic(sourceError(r.table, k, "invalid timestamp"))
	}
	t = t.UTC()
	return &t
}
func (r row) at(k string) time.Time {
	v := r.timestamp(k)
	if v == nil {
		panic(sourceError(r.table, k, "timestamp is required"))
	}
	return *v
}
func (r row) blob(k string) []byte {
	v := r.text(k)
	if v == "" {
		return nil
	}
	if !strings.HasPrefix(v, `\x`) {
		panic(sourceError(r.table, k, "expected PostgreSQL hex BYTEA"))
	}
	b, err := hex.DecodeString(v[2:])
	if err != nil {
		panic(sourceError(r.table, k, "invalid BYTEA"))
	}
	return b
}
func (r row) decode(k string, out any) {
	v := r.text(k)
	if v == "" {
		return
	}
	if json.Unmarshal([]byte(v), out) != nil {
		panic(sourceError(r.table, k, "invalid JSON structure"))
	}
}
func decodeRow(table, data string) row {
	r := row{table: table}
	if json.Unmarshal([]byte(data), &r.data) != nil || r.data == nil {
		panic(sourceError(table, "row", "invalid JSON object"))
	}
	return r
}

func (m *importer) write(v any) error {
	return logging.Wrap(m.db.Create(v).Error, "insert converted "+reflect.TypeOf(v).String())
}
func (m *importer) exec(sql string, args ...any) error {
	return logging.Wrap(m.db.Exec(sql, args...).Error, "update migration data")
}
func (m *importer) walk(table string, fn func(row) error) error {
	rows, err := m.db.Raw(`SELECT data FROM migration_source WHERE table_name=? ORDER BY COALESCE(json_extract(data,'$.started_at'),json_extract(data,'$.created_at'),json_extract(data,'$.first_seen_at'),json_extract(data,'$.occurred_at'),json_extract(data,'$.received_at'),''), old_id, data`, table).Rows()
	if err != nil {
		return logging.WithStack(err)
	}
	defer rows.Close()
	for rows.Next() {
		if err = m.ctx.Err(); err != nil {
			return logging.WithStack(err)
		}
		var data string
		if err = rows.Scan(&data); err != nil {
			return logging.WithStack(err)
		}
		if err = fn(decodeRow(table, data)); err != nil {
			return err
		}
	}
	return logging.WithStack(rows.Err())
}
func (m *importer) one(table, field, id string) row {
	var data []string
	column := sourceColumn(field)
	err := m.db.Raw("SELECT data FROM migration_source WHERE table_name=? AND "+column+"=?", table, id).Scan(&data).Error
	if err != nil {
		panic(logging.WithStack(err))
	}
	if len(data) != 1 {
		panic(sourceError(table, field, "expected exactly one related row"))
	}
	return decodeRow(table, data[0])
}
func (m *importer) related(table, field, id string, fn func(row) error) error {
	var data []string
	err := m.db.Raw("SELECT data FROM migration_source WHERE table_name=? AND "+sourceColumn(field)+"=? ORDER BY COALESCE(json_extract(data,'$.appearance_order'),0), old_id, data", table, id).Scan(&data).Error
	if err != nil {
		return logging.WithStack(err)
	}
	for _, v := range data {
		if err = fn(decodeRow(table, v)); err != nil {
			return err
		}
	}
	return nil
}
func (m *importer) id(table, old string) int64 {
	if id := m.ids[table][old]; id > 0 {
		return id
	}
	panic(sourceError(table, "id", "missing related source row"))
}
func sourceColumn(field string) string {
	if field == "id" {
		return "old_id"
	}
	switch field {
	case "watch_id", "post_id", "tid":
		return "json_extract(data,'$." + field + "')"
	}
	panic(sourceError("migration_source", "lookup", "unsupported lookup field"))
}
func (m *importer) decrypt(r row, field, aad string) string {
	b := r.blob(field)
	if len(b) < 29 || (b[0] != 1 && b[0] != 2) {
		panic(sourceError(r.table, field, "unsupported encrypted payload"))
	}
	var associated []byte
	if b[0] == 2 {
		if aad == "" {
			panic(sourceError(r.table, field, "missing v2 field context"))
		}
		associated = []byte(aad)
	}
	plain, err := m.old.Open(nil, b[1:13], b[13:], associated)
	if err != nil {
		panic(sourceError(r.table, field, "decryption failed; check NGA_MIGRATE_OLD_KEY"))
	}
	if !utf8.Valid(plain) {
		panic(sourceError(r.table, field, "decrypted data is not UTF-8"))
	}
	m.report.Converted["credentials_verified"]++
	return string(plain)
}
func (m *importer) seal(purpose string, value any) []byte {
	b, err := json.Marshal(value)
	if err != nil {
		panic(logging.WithStack(err))
	}
	encrypted, err := m.cipher.Seal(m.ctx, purpose, b)
	if err != nil {
		panic(err)
	}
	plain, err := m.cipher.Open(m.ctx, purpose, encrypted)
	if err != nil || string(plain) != string(b) {
		panic(logging.WithStack(errors.New("target credential verification failed")))
	}
	return encrypted
}

func convert(ctx context.Context, o options) (err error) {
	ctx, span := logging.Start(ctx, "migration.convert_sqlite")
	defer span.End(&err)
	defer func() {
		if v := recover(); v != nil {
			err = logging.FromPanic(v)
		}
	}()
	if err = unused(o.Output); err != nil {
		return err
	}
	if err = unused(o.Output + ".report.json"); err != nil {
		return err
	}
	newKey := os.Getenv("NGA_REMINDER_ENCRYPTION_KEY")
	oldKey := os.Getenv("NGA_MIGRATE_OLD_KEY")
	if oldKey == "" {
		oldKey = newKey
	}
	key, e := base64.StdEncoding.DecodeString(oldKey)
	if e != nil || len(key) != 32 {
		return logging.WithStack(errors.New("NGA_MIGRATE_OLD_KEY must decode to 32 bytes"))
	}
	block, e := aes.NewCipher(key)
	if e != nil {
		return logging.WithStack(e)
	}
	old, e := cipher.NewGCM(block)
	if e != nil {
		return logging.WithStack(e)
	}
	newCipher, e := infrastructure.NewCredentialCipher(newKey)
	if e != nil {
		return e
	}
	file, e := os.CreateTemp(filepath.Dir(o.Output), ".sqlite-migrate-*")
	if e != nil {
		return logging.WithStack(e)
	}
	name := file.Name()
	if e = file.Close(); e != nil {
		return logging.WithStack(e)
	}
	defer func() {
		for _, suffix := range []string{"", "-wal", "-shm"} {
			_ = os.Remove(name + suffix)
		}
	}()
	store, e := repository.Open(ctx, name)
	if e != nil {
		return e
	}
	if e = store.Close(ctx); e != nil {
		return e
	}
	dsn := url.URL{Scheme: "file", Path: name}
	db, e := gorm.Open(sqlite.Open(dsn.String()), &gorm.Config{Logger: logger.Discard})
	if e != nil {
		return logging.WithStack(e)
	}
	pool, e := db.DB()
	if e != nil {
		return logging.WithStack(e)
	}
	defer pool.Close()
	m := &importer{ctx: ctx, db: db.WithContext(ctx), old: old, cipher: newCipher, ids: map[string]map[string]int64{}, o: o}
	m.report = report{Source: map[string]int64{}, Target: map[string]int64{}, Converted: map[string]int64{}, Runtime: map[string]string{"NGA_REMINDER_TIMEZONE": o.Timezone, "NGA_REMINDER_BACKGROUND_ENABLED": "false"}, Notes: []string{"完整源数据及未映射字段保留在 PG JSONL 快照；SQLite 仅保留 Go 使用的业务数据。", "assets 文件未读取、复制、重命名或删除；上线后挂载原目录，并用资源扫描核对缺失文件。", "旧 assets.max_download_bytes 与 persistence.store_raw_payload 不在数据库中；请按旧部署配置设置 NGA_REMINDER_MAX_DOWNLOAD_BYTES（默认 10 MiB）和 NGA_REMINDER_STORE_RAW_PAYLOAD（默认 false）。"}}
	err = m.db.Transaction(func(tx *gorm.DB) error {
		m.db = tx
		if e = m.load(o.Source); e != nil {
			return e
		}
		for _, table := range []string{"watch_targets", "posts", "notification_channels", "crawl_runs", "post_events", "bot_bindings", "uid_reply_backfill_jobs", "notification_outbox", "system_alerts", "system_alert_outbox"} {
			m.ids[table] = map[string]int64{}
			if e = m.walk(table, func(r row) error {
				id := r.text("id")
				if id == "" || m.ids[table][id] != 0 {
					return sourceError(table, "id", "missing or duplicate ID")
				}
				m.ids[table][id] = int64(len(m.ids[table]) + 1)
				if table == "system_alert_outbox" {
					m.ids[table][id] += int64(len(m.ids["notification_outbox"]))
				}
				return nil
			}); e != nil {
				return e
			}
		}
		for _, stage := range []struct {
			name string
			run  func() error
		}{
			{"accounts", m.accounts}, {"integrations", m.channels}, {"bot", m.bot}, {"watches", m.watches}, {"content", m.content}, {"events", m.events}, {"alerts", m.alerts}, {"history", m.history}, {"renewal", m.renewal}, {"validation", m.validate},
		} {
			zerolog.Ctx(ctx).Info().Str("stage", stage.name).Msg("Migration stage started")
			if e = stage.run(); e != nil {
				return logging.Wrap(e, "convert "+stage.name)
			}
			zerolog.Ctx(ctx).Info().Str("stage", stage.name).Msg("Migration stage finished")
		}
		return m.exec("DROP TABLE migration_source")
	})
	if err != nil {
		return err
	}
	m.db = db.WithContext(ctx)
	for _, sql := range []string{"PRAGMA wal_checkpoint(TRUNCATE)", "PRAGMA journal_mode=DELETE", "VACUUM"} {
		if err = m.exec(sql); err != nil {
			return err
		}
	}
	if err = pool.Close(); err != nil {
		return logging.WithStack(err)
	}
	file, err = os.OpenFile(name, os.O_RDWR, 0600)
	if err != nil {
		return logging.WithStack(err)
	}
	err = file.Sync()
	closeErr := file.Close()
	if err != nil || closeErr != nil {
		return logging.WithStack(errors.Join(err, closeErr))
	}
	m.report.Ready = true
	m.report.IDs = m.ids
	reportFile, e := os.CreateTemp(filepath.Dir(o.Output), ".migration-report-*")
	if e != nil {
		return logging.WithStack(e)
	}
	defer os.Remove(reportFile.Name())
	defer reportFile.Close()
	encoder := json.NewEncoder(reportFile)
	encoder.SetIndent("", "  ")
	if err = encoder.Encode(m.report); err != nil {
		return logging.WithStack(err)
	}
	if err = reportFile.Sync(); err != nil {
		return logging.WithStack(err)
	}
	if err = reportFile.Close(); err != nil {
		return logging.WithStack(err)
	}
	if err = publish(name, o.Output); err != nil {
		return err
	}
	return publish(reportFile.Name(), o.Output+".report.json")
}

func (m *importer) load(path string) error {
	file, err := os.Open(path)
	if err != nil {
		return logging.Wrap(err, "open PG snapshot")
	}
	defer file.Close()
	hash := sha256.New()
	decoder := json.NewDecoder(io.TeeReader(file, hash))
	var rec sourceRecord
	if err = decoder.Decode(&rec); err != nil {
		return logging.Wrap(err, "read snapshot header")
	}
	if rec.Header == nil || rec.Header.Format != "nga-rust-pg-v1" || rec.Header.At.IsZero() {
		return logging.WithStack(errors.New("unsupported PG snapshot header"))
	}
	h := rec.Header
	m.report.SnapshotAt = h.At.UTC()
	columns := map[string][]string{}
	for _, table := range h.Tables {
		if _, ok := columns[table.Name]; ok {
			return sourceError(table.Name, "schema", "duplicate table")
		}
		columns[table.Name] = table.Columns
		m.report.Source[table.Name] = 0
	}
	for _, line := range strings.Split(strings.TrimSpace(requiredSchema), "\n") {
		fields := strings.Fields(line)
		for _, field := range fields[1:] {
			if !slices.Contains(columns[fields[0]], field) {
				return sourceError(fields[0], field, "missing schema column; expected Rust migrations 0001-0007")
			}
		}
	}
	if err = m.exec("CREATE TABLE migration_source (seq INTEGER PRIMARY KEY, table_name TEXT NOT NULL, old_id TEXT NOT NULL, data TEXT NOT NULL)"); err != nil {
		return err
	}
	if err = m.exec("CREATE INDEX migration_source_lookup ON migration_source(table_name,old_id)"); err != nil {
		return err
	}
	for {
		rec = sourceRecord{}
		if err = decoder.Decode(&rec); err != nil {
			return logging.Wrap(err, "read complete PG snapshot")
		}
		if rec.Complete {
			if !reflect.DeepEqual(rec.Counts, m.report.Source) {
				return logging.WithStack(errors.New("PG snapshot row counts do not match"))
			}
			break
		}
		if _, ok := columns[rec.Table]; !ok || rec.Header != nil || len(rec.Row) == 0 {
			return logging.WithStack(errors.New("invalid PG snapshot row"))
		}
		r := decodeRow(rec.Table, string(rec.Row))
		if len(r.data) != len(columns[rec.Table]) {
			return sourceError(rec.Table, "row", "row does not match exported columns")
		}
		for _, field := range columns[rec.Table] {
			if _, ok := r.data[field]; !ok {
				return sourceError(rec.Table, field, "missing exported column value")
			}
		}
		id := ""
		if v := r.data["id"]; len(v) > 0 && v[0] == '"' {
			id = r.text("id")
		}
		if err = m.exec("INSERT INTO migration_source(table_name,old_id,data) VALUES(?,?,?)", rec.Table, id, string(rec.Row)); err != nil {
			return err
		}
		m.report.Source[rec.Table]++
	}
	if err = decoder.Decode(&rec); !errors.Is(err, io.EOF) {
		return logging.WithStack(errors.New("unexpected data after snapshot footer"))
	}
	m.report.SnapshotSHA256 = hex.EncodeToString(hash.Sum(nil))
	for _, field := range []string{"watch_id", "post_id", "tid"} {
		if err = m.exec("CREATE INDEX migration_source_" + field + " ON migration_source(table_name," + sourceColumn(field) + ")"); err != nil {
			return err
		}
	}
	if _, ok := columns["_sqlx_migrations"]; ok {
		if err = m.walk("_sqlx_migrations", func(r row) error {
			if !r.yes("success") || r.number("version") > 7 {
				return sourceError("_sqlx_migrations", "version", "unsupported or failed migration")
			}
			return nil
		}); err != nil {
			return err
		}
	}
	return nil
}

const requiredSchema = `
nga_accounts id passport_uid_encrypted passport_cid_encrypted cookie_encrypted status encryption_version last_auth_checked_at
threads tid fid title forum_name author_uid author_name coverage remote_total_pages remote_vrows first_seen_at last_seen_at
posts id tid pid floor_number post_kind parent_post_id author_uid author_name subject content_raw published_at_unix page_number raw_payload first_seen_at
watch_targets id target_type target_id target_name enabled pause_reason status baseline_completed interval_seconds schedule_json no_fetch_periods_json next_run_at deleted_at created_at updated_at
thread_watch_options watch_id history_mode history_parallel_enabled history_parallelism
watch_cursors watch_id last_floor remote_vrows remote_total_pages
user_watch_cursors watch_id newest_topic_at_unix newest_reply_at_unix newest_topic_tid newest_reply_pid
crawl_runs id watch_id status baseline sync_mode trigger_kind pages_requested posts_inserted started_at completed_at error_kind
platform_integrations id platform credentials_encrypted bot_enabled delivery_enabled enabled
notification_channels id integration_id label target_encrypted enabled
bot_bindings id integration_id actor_id conversation_id conversation_type role enabled created_at
bot_inbound_events id integration_id platform_message_id status received_at
post_events id post_id event_type read_at occurred_at
post_event_watch_matches post_event_id watch_id
system_alerts id alert_key title body url resolved_at created_at updated_at
system_alert_outbox id alert_id channel_id status attempt_count next_attempt_at created_at delivered_at
notification_outbox id post_event_id channel_id status attempt_count next_attempt_at created_at delivered_at
watch_notification_authors watch_id author_uid
watch_notification_channels watch_id channel_id
assets id source_url original_name mime_type size_bytes local_relative_path download_status
post_assets post_id asset_id appearance_order
nga_account_renewal_settings account_id enabled login_name_encrypted password_encrypted bot_binding_id credential_status
nga_login_sessions id account_id bot_binding_id status created_at expires_at
thread_floor_gaps watch_id floor_number page_hint retry_count first_detected_at expires_at status
uid_reply_backfill_jobs id watch_id uid start_at_unix end_at_unix status pages_requested candidates_processed posts_inserted completed_at error_kind
`
