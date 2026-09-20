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
