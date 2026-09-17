package main

import (
	"bytes"
	"context"
	"crypto/aes"
	"crypto/cipher"
	"crypto/rand"
	"encoding/base64"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
)

var fixtureOldKey = base64.StdEncoding.EncodeToString(bytes.Repeat([]byte{7}, 32))
var fixtureNewKey = base64.StdEncoding.EncodeToString(bytes.Repeat([]byte{9}, 32))

// 使用真实归档 schema 和 Rust 的 AES-GCM 布局；只生成假凭据。
func encryptedFixture(t *testing.T, plain, aad string) []byte {
	t.Helper()
	key, _ := base64.StdEncoding.DecodeString(fixtureOldKey)
	block, err := aes.NewCipher(key)
	if err != nil {
		t.Fatal(err)
	}
	aead, err := cipher.NewGCM(block)
	if err != nil {
		t.Fatal(err)
	}
	nonce := make([]byte, 12)
	if _, err = rand.Read(nonce); err != nil {
		t.Fatal(err)
	}
	version := byte(1)
	if aad != "" {
		version = 2
	}
	return aead.Seal(append([]byte{version}, nonce...), nonce, []byte(plain), []byte(aad))
}

func preparePG(t *testing.T) (*pgx.Conn, string, time.Time) {
	t.Helper()
	dsn := os.Getenv("NGA_MIGRATE_TEST_PG_URL")
	if dsn == "" {
		t.Skip("set NGA_MIGRATE_TEST_PG_URL to a disposable PostgreSQL database")
	}
	ctx := context.Background()
	conn, err := pgx.Connect(ctx, dsn)
	if err != nil {
		t.Fatal(err)
	}
	schema := fmt.Sprintf("migration_fixture_%d", time.Now().UnixNano())
	exec := func(sql string, args ...any) {
		t.Helper()
		if _, e := conn.Exec(ctx, sql, args...); e != nil {
			t.Fatal(e)
		}
	}
	exec("CREATE SCHEMA " + pgx.Identifier{schema}.Sanitize())
	t.Cleanup(func() {
		_, e := conn.Exec(context.Background(), "DROP SCHEMA "+pgx.Identifier{schema}.Sanitize()+" CASCADE")
		if e != nil {
			t.Error(e)
		}
		_ = conn.Close(context.Background())
	})
	exec("SET search_path TO " + pgx.Identifier{schema}.Sanitize())
	files, err := filepath.Glob("../../../archive/rust-service/service/migrations/postgres/*.sql")
	if err != nil || len(files) != 7 {
		t.Fatal("expected seven archived PG migrations", err, len(files))
	}
	for _, file := range files {
		data, e := os.ReadFile(file)
		if e != nil {
			t.Fatal(e)
		}
		exec(string(data))
	}
	exec("CREATE TABLE _sqlx_migrations(version bigint PRIMARY KEY,success boolean NOT NULL); INSERT INTO _sqlx_migrations SELECT generate_series(1,7),true")
	exec(`CREATE TABLE legacy_extra (id bigint, note text, payload jsonb); INSERT INTO legacy_extra VALUES(9007199254740993,'中文完整快照','{"retained":true}')`)
	now := time.Now().UTC().Truncate(time.Second)
	earlier := now.Add(-time.Hour)
	exec(`INSERT INTO nga_accounts(id,label,passport_uid_encrypted,passport_cid_encrypted,cookie_encrypted,status,last_auth_checked_at,created_at,updated_at) VALUES('account','fixture',$1,$2,$3,'valid',$4,$5,$5)`, encryptedFixture(t, "2001", ""), encryptedFixture(t, "fake-cid-secret", ""), encryptedFixture(t, "ngaPassportUid=2001; ngaPassportCid=fake-cid-secret; extra=fake-extra-cookie", ""), now, earlier)
	exec(`INSERT INTO threads(tid,fid,title,forum_name,author_uid,author_name,first_seen_at,last_seen_at) VALUES(1001,3001,'迁移测试主题','测试版块',2001,'测试作者',$1,$1),(1002,3001,'仅有主题元数据','测试版块',2001,'测试作者',$1,$1)`, earlier)
	exec(`INSERT INTO posts(id,tid,pid,floor_number,post_kind,parent_post_id,author_uid,author_name,subject,content_raw,published_at_unix,page_number,raw_payload,first_seen_at) VALUES
	 ('topic',1001,NULL,0,'topic',NULL,2001,'测试作者','迁移测试主题','[b]完整中文正文[/b][img]https://img.nga.cn/fixture.png[/img]',1767225600,1,'{}',$1),
	 ('reply',1001,4001,1,'reply',NULL,2002,'回复作者','','已有回复',1767225660,1,'{}',$1),
	 ('comment',1001,5001,NULL,'comment','reply',2003,'评论作者','','已有评论',1767225670,1,'{"comment_to_id":4001}',$1),
	 ('wide-id',1001,9007199254740993,2,'reply',NULL,9007199254740993,'大整数作者','','64 位整数',NULL,1,'{}',$1)`, earlier)
	for i, id := range []string{"tid", "uid", "manual-paused", "auth-paused", "deleted"} {
		kind, target := "thread", int64(1001+i)
		if id == "uid" {
			kind, target = "user", 2002
		}
		enabled, pause, status := 1, any(nil), "active"
		if id == "manual-paused" {
			enabled, pause, status = 0, "user", "paused"
		}
		if id == "auth-paused" {
			enabled, pause, status = 0, "auth", "paused"
		}
		var deleted any
		if id == "deleted" {
			deleted = now
		}
		exec(`INSERT INTO watch_targets(id,target_type,target_id,target_name,enabled,pause_reason,status,baseline_completed,interval_seconds,schedule_json,no_fetch_periods_json,next_run_at,created_at,updated_at,deleted_at) VALUES($1,$2,$3,$4,$5,$6,$7,1,120,$8,$9,$10,$11,$11,$12)`, id, kind, target, "监控 "+id, enabled, pause, status, `[{"days":["weekends"],"start_time":"00:00","end_time":"23:59","interval":3600}]`, `[{"days":["Sunday"],"start_time":"23:00","end_time":"24:00"}]`, now.Add(time.Hour), earlier.Add(time.Duration(i)*time.Second), deleted)
		if kind == "thread" {
			mode := "full"
			if id == "manual-paused" {
				mode = "incremental"
			}
			exec("INSERT INTO thread_watch_options(watch_id,history_mode) VALUES($1,$2)", id, mode)
			exec("INSERT INTO watch_cursors(watch_id,last_floor) VALUES($1,3)", id)
		} else {
			exec("INSERT INTO user_watch_cursors(watch_id,newest_topic_at_unix,newest_topic_tid,newest_reply_at_unix,newest_reply_pid) VALUES($1,1767225600,1001,1767225660,4001)", id)
		}
	}
	exec("UPDATE thread_watch_options SET history_parallel_enabled=1, history_parallelism=4 WHERE watch_id='tid'")
	exec(`INSERT INTO platform_integrations(id,platform,label,enabled,delivery_enabled,bot_enabled,credentials_encrypted) VALUES('bark','bark','Bark',1,1,0,$1),('feishu','feishu','Feishu',1,1,1,$2)`, encryptedFixture(t, `{"platform":"bark","credentials":{"server_url":"https://api.day.app","group":"fixture-group"}}`, ""), encryptedFixture(t, `{"platform":"feishu","credentials":{"app_id":"fake-app-id","app_secret":"fake-app-secret"}}`, ""))
	exec(`INSERT INTO notification_channels(id,integration_id,label,target_encrypted) VALUES('bark','bark','Bark target',$1),('feishu','feishu','Feishu target',$2)`, encryptedFixture(t, `{"platform":"bark","target":{"device_key":"fake-device-key"}}`, ""), encryptedFixture(t, `{"platform":"feishu","target":{"receive_id":"fake-recipient","receive_id_type":"chat_id"}}`, ""))
	exec(`INSERT INTO bot_bindings(id,integration_id,actor_id,conversation_id,conversation_type,role,label,created_at) VALUES('owner','feishu','fake-owner','fake-private-chat','private','owner','owner',$1),('group','feishu','fake-owner','fake-group-chat','group','owner','group',$1)`, earlier)
	exec(`INSERT INTO bot_pairing_tokens(id,integration_id,token_hash,requested_role,expires_at) VALUES('old-code','feishu','unused-hash','owner',$1)`, now.Add(time.Hour))
	exec(`INSERT INTO bot_inbound_events(id,integration_id,platform_message_id,actor_id,conversation_id,conversation_type,status,received_at) VALUES('message','feishu','fake-message-id','fake-owner','fake-private-chat','private','succeeded',$1)`, earlier)
	exec(`INSERT INTO bot_outbox(id,dedupe_key,integration_id,conversation_id,message_kind,payload_encrypted,status) VALUES('reply','old-reply','feishu','fake-private-chat','text',$1,'pending')`, encryptedFixture(t, `{"text":"old reply"}`, ""))
	exec(`INSERT INTO watch_notification_channels(watch_id,channel_id) VALUES('tid','bark'),('uid','feishu'); INSERT INTO watch_notification_authors(watch_id,author_uid) VALUES('uid',2002)`)
	exec(`INSERT INTO post_events(id,post_id,event_type,read_at,occurred_at) VALUES('read','reply','new_reply',$1,$1),('unread','comment','new_reply',NULL,$1)`, earlier)
	exec(`INSERT INTO post_event_watch_matches(post_event_id,watch_id) VALUES('read','tid'),('read','uid'),('unread','deleted')`)
	exec(`INSERT INTO notification_outbox(id,post_event_id,channel_id,status,attempt_count,delivered_at,created_at,next_attempt_at) VALUES('sent','read','bark','delivered',2,$1,$1,$1),('pending','unread','feishu','sending',1,NULL,$1,$1)`, earlier)
	exec(`INSERT INTO notification_deliveries(id,outbox_id,attempt,success,response_summary) VALUES('delivery-log','sent',2,1,'preserved legacy response')`)
	exec(`INSERT INTO assets(id,source_url,mime_type,size_bytes,local_relative_path,download_status,first_seen_at) VALUES('local','https://img.nga.cn/fixture.png','image/png',12,$1,'ready',$2),('missing','https://img.nga.cn/missing.png','image/png',12,'ff/missing.png','ready',$2),('remote','https://img.nga.cn/remote.png',NULL,NULL,NULL,'remote_only',$2)`, "ab/"+strings.Repeat("a", 64)+".png", earlier)
	exec(`INSERT INTO post_assets(post_id,asset_id,appearance_order) VALUES('topic','local',0),('topic','missing',1),('reply','remote',0)`)
	exec(`INSERT INTO nga_account_renewal_settings(account_id,enabled,login_name_encrypted,password_encrypted,bot_binding_id) VALUES('account',1,$1,$2,'owner')`, encryptedFixture(t, "fake-login-name", "nga_account:account:renewal_login:v2"), encryptedFixture(t, "fake-login-password", "nga_account:account:renewal_password:v2"))
	exec(`INSERT INTO nga_login_sessions(id,account_id,bot_binding_id,integration_id,actor_id,conversation_id,trigger_kind,status,protocol_context_encrypted,created_at,expires_at) VALUES('login','account','owner','feishu','fake-owner','fake-private-chat','manual','awaiting_captcha',$1,$2,$3)`, []byte("discarded-protocol-context"), earlier, now.Add(time.Hour))
	exec(`INSERT INTO crawl_runs(id,watch_id,status,baseline,sync_mode,pages_requested,posts_inserted,started_at,completed_at,trigger_kind) VALUES('done','tid','succeeded',0,'incremental',1,4,$1,$2,'scheduled'),('running','deleted','running',0,'incremental',2,3,$2,NULL,'manual')`, earlier, now)
	exec(`INSERT INTO thread_floor_gaps(watch_id,floor_number,page_hint,status,retry_count,first_detected_at,next_retry_at,expires_at) VALUES('tid',3,1,'pending',0,$1,$2,$3),('deleted',2,1,'pending',2,$4,$4,$4)`, now, now.Add(5*time.Second), now.Add(120*time.Second), earlier)
	exec(`INSERT INTO uid_reply_backfill_jobs(id,watch_id,uid,start_date,start_at_unix,end_at_unix,status,pages_requested,candidates_processed,posts_inserted,created_at,updated_at) VALUES('backfill','uid',2002,'2026-01-01',1767225600,1767225700,'running',1,2,1,$1,$1)`, earlier)
	t.Setenv("NGA_MIGRATE_PG_URL", dsn)
	t.Setenv("NGA_MIGRATE_OLD_KEY", fixtureOldKey)
	t.Setenv("NGA_REMINDER_ENCRYPTION_KEY", fixtureNewKey)
	return conn, schema, now
}
