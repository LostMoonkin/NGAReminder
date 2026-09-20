package repository

import (
	"context"
	"database/sql"
	"fmt"
	"path/filepath"
	"testing"
)

func TestUpgradeSpec02Database(t *testing.T) {
	path := filepath.Join(t.TempDir(), "legacy-go.db")
	db, err := sql.Open("sqlite", path)
	if err != nil {
		t.Fatal(err)
	}
	for _, statement := range []string{
		fmt.Sprintf("PRAGMA application_id = %d", applicationID),
		`CREATE TABLE watches (id integer PRIMARY KEY AUTOINCREMENT, tid integer, label text, title text, init_mode text, paused numeric, state text, baseline_complete numeric, cursor_floor integer, history_floor integer, history_before datetime, created_at datetime, updated_at datetime)`,
		`CREATE UNIQUE INDEX idx_watches_tid ON watches(tid)`,
		`INSERT INTO watches (tid,label,title,init_mode,paused,state,baseline_complete,cursor_floor,history_floor) VALUES (1001,'fixture','fixture','from_now',1,'ready',1,17,7)`,
	} {
		if _, err = db.Exec(statement); err != nil {
			t.Fatal(err)
		}
	}
	if err = db.Close(); err != nil {
		t.Fatal(err)
	}
	store, err := Open(context.Background(), path)
	if err != nil {
		t.Fatal(err)
	}
	watch, err := store.Watch(context.Background(), 1)
	if err != nil || watch.Kind != "tid" || watch.TID != 1001 || watch.IntervalSeconds != 60 || watch.HistoryConcurrency != 1 || !watch.Paused || !watch.BaselineComplete || watch.CursorFloor != 17 || watch.HistoryFloor != 7 || watch.RemoteRows != 0 || watch.RemoteTotalPages != 0 {
		t.Fatalf("upgrade changed existing TID progress: %+v %v", watch, err)
	}
	for _, uid := range []int64{2001, 2002} {
		watch := Watch{Kind: "uid", UID: uid, InitMode: "from_now", State: "ready"}
		if err = store.SaveWatch(context.Background(), &watch); err != nil {
			t.Fatalf("old TID index still rejects UID watches: %v", err)
		}
	}
	duplicate := Watch{Kind: "tid", TID: 1001}
	if err = store.SaveWatch(context.Background(), &duplicate); err == nil {
		t.Fatal("TID uniqueness was lost")
	}
	duplicate = Watch{Kind: "uid", UID: 2001}
	if err = store.SaveWatch(context.Background(), &duplicate); err == nil {
		t.Fatal("UID uniqueness was lost")
	}
	if err = store.Close(context.Background()); err != nil {
		t.Fatal(err)
	}
	store, err = Open(context.Background(), path)
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close(context.Background())
	watches, err := store.Watches(context.Background())
	if err != nil || len(watches) != 3 {
		t.Fatalf("upgrade is not repeatable: %d %v", len(watches), err)
	}
}

func TestUpgradeParityPreservesExistingGoData(t *testing.T) {
	ctx := context.Background()
	path := filepath.Join(t.TempDir(), "previous-go.db")
	store, err := Open(ctx, path)
	if err != nil {
		t.Fatal(err)
	}
	watch := Watch{Kind: "tid", TID: 1001, Title: "Latest observed title", State: "ready", InitMode: "full", BaselineComplete: true, CursorFloor: 100, RemoteRows: 101, RemoteTotalPages: 6}
	if err = store.SaveWatch(ctx, &watch); err != nil {
		t.Fatal(err)
	}
	uid := Watch{Kind: "uid", UID: 2001, Title: "Profile", State: "missing", BaselineComplete: true, ReplyCursor: UserCursor{ID: 4001, Timestamp: 100}}
	if err = store.SaveWatch(ctx, &uid); err != nil {
		t.Fatal(err)
	}
	if err = store.SaveRun(ctx, &Run{WatchID: uid.ID, Status: "missing", Error: "NGA 主题不存在，已停止自动采集"}); err != nil {
		t.Fatal(err)
	}
	posts := []Post{{TID: 1001, Key: "main", Kind: "main", AuthorUID: 2001, Author: "Author", Subject: "Old main title", Body: "Main", SourceURL: "https://bbs.nga.cn/read.php?tid=1001"},
		{TID: 1001, Key: "pid:4001", PID: 4001, Kind: "reply", Floor: 100, AuthorUID: 2001, Author: "Author", Body: "Reply", SourceURL: "https://bbs.nga.cn/read.php?tid=1001&pid=4001"}}
	if _, err = store.InsertPosts(ctx, posts); err != nil {
		t.Fatal(err)
	}
	if _, err = store.InsertPosts(ctx, []Post{{TID: 1002, Key: "pid:5001", PID: 5001, Kind: "reply", AuthorUID: 2001, Subject: "Existing UID thread title"}}); err != nil {
		t.Fatal(err)
	}
	post, _ := store.PostByKey(ctx, 1001, "pid:4001")
	event, err := store.MatchEvent(ctx, post.ID, uid)
	if err != nil {
		t.Fatal(err)
	}
	channel := Channel{Name: "Stored channel", Kind: "bark", Enabled: true}
	if err = store.SaveChannel(ctx, &channel); err != nil {
		t.Fatal(err)
	}
	if err = store.Enqueue(ctx, event.ID, channel); err != nil {
		t.Fatal(err)
	}
	if err = store.Close(ctx); err != nil {
		t.Fatal(err)
	}
	db, err := sql.Open("sqlite", path)
	if err != nil {
		t.Fatal(err)
	}
	// 模拟本次修复前的实际 Go 表形状，保留原业务行。
	for _, statement := range []string{"DROP TABLE threads", "DROP TABLE system_alerts", "ALTER TABLE posts DROP COLUMN page_number", "ALTER TABLE posts DROP COLUMN raw_payload", "DROP INDEX delivery_target", "ALTER TABLE deliveries DROP COLUMN alert_id", "CREATE UNIQUE INDEX delivery_target ON deliveries(event_id,channel_id)", "ALTER TABLE event_watches DROP COLUMN kind", "ALTER TABLE resources DROP COLUMN original_name"} {
		if _, err = db.Exec(statement); err != nil {
			t.Fatal(statement, err)
		}
	}
	if err = db.Close(); err != nil {
		t.Fatal(err)
	}
	for attempt := 0; attempt < 2; attempt++ {
		store, err = Open(ctx, path)
		if err != nil {
			t.Fatal(err)
		}
		thread, err := store.Thread(ctx, 1001)
		if err != nil || thread.Title != "Latest observed title" || thread.AuthorUID != 2001 || thread.RemoteRows != 101 || thread.RemoteTotalPages != 6 {
			t.Fatal("upgrade omitted known thread metadata", thread, err)
		}
		partial, err := store.Thread(ctx, 1002)
		if err != nil || partial.Title != "Existing UID thread title" {
			t.Fatal("reply-only legacy title omitted", partial, err)
		}
		saved, err := store.PostByKey(ctx, 1001, "pid:4001")
		if err != nil || saved.ID != post.ID || saved.Body != "Reply" || saved.PageNumber != 0 || len(saved.RawPayload) != 0 || saved.WebURL() != post.SourceURL {
			t.Fatal("upgrade changed content or invented provenance", saved, err)
		}
		restored, _ := store.Watch(ctx, uid.ID)
		if restored.State != "ready" || restored.ReplyCursor.ID != 4001 {
			t.Fatal("known C02 misclassification was not repaired", restored)
		}
		oldWatch, _ := store.Watch(ctx, watch.ID)
		if oldWatch.CursorFloor != 100 || !oldWatch.BaselineComplete {
			t.Fatal("upgrade reset progress")
		}
		oldEvent, err := store.Event(ctx, event.ID)
		if err != nil || len(oldEvent.Sources) != 1 || oldEvent.Sources[0].Kind != "uid" || len(oldEvent.Deliveries) != 1 || oldEvent.Deliveries[0].EventID != event.ID {
			t.Fatal("upgrade lost notification relations", oldEvent, err)
		}
		if err = store.Transaction(ctx, func(ctx context.Context, tx *Store) error { return tx.EnsureAuthAlert(ctx) }); err != nil {
			t.Fatal(err)
		}
		alerts, err := store.Alerts(ctx, 1)
		if err != nil || len(alerts) != 1 || len(alerts[0].Deliveries) != 1 {
			t.Fatal("new delivery index rejected or duplicated alert", alerts, err)
		}
		if err = store.Close(ctx); err != nil {
			t.Fatal(err)
		}
	}
}
