package infrastructure

import (
	"context"
	"os"
	"path/filepath"
	"testing"
)

func TestSQLiteDiskUsage(t *testing.T) {
	databasePath := filepath.Join(t.TempDir(), "fixture.db")
	for name, data := range map[string][]byte{
		databasePath:          []byte("db"),
		databasePath + "-wal": []byte("wal"),
		databasePath + "-shm": []byte("shm!"),
	} {
		if err := os.WriteFile(name, data, 0600); err != nil {
			t.Fatal(err)
		}
	}
	usage, err := SQLiteDiskUsage(context.Background(), databasePath)
	if err != nil || usage != 9 {
		t.Fatalf("SQLite usage = %d, want 9: %v", usage, err)
	}
	if err = os.Remove(databasePath + "-wal"); err != nil {
		t.Fatal(err)
	}
	if err = os.Remove(databasePath + "-shm"); err != nil {
		t.Fatal(err)
	}
	usage, err = SQLiteDiskUsage(context.Background(), databasePath)
	if err != nil || usage != 2 {
		t.Fatalf("SQLite usage without sidecars = %d, want 2: %v", usage, err)
	}
}
