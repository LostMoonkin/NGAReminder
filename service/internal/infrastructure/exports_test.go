package infrastructure

import (
	"context"
	"os"
	"path/filepath"
	"testing"
)

func TestExportsRejectsTraversalAndSymlinks(t *testing.T) {
	ctx := context.Background()
	root, outside := t.TempDir(), t.TempDir()
	store := &Exports{Path: root}

	keep := filepath.Join(outside, "keep.md")
	if err := os.WriteFile(keep, []byte("outside"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(keep, filepath.Join(root, "link.md")); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(keep, filepath.Join(root, ".tmp-link.md")); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, ".tmp-stale.md"), []byte("partial"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, "export-1.md"), []byte("done"), 0600); err != nil {
		t.Fatal(err)
	}

	if _, _, err := store.Open(ctx, "../keep.md"); err == nil {
		t.Fatal("export traversal was accepted")
	}
	if _, _, err := store.Open(ctx, "link.md"); err == nil {
		t.Fatal("export symlink open was accepted")
	}
	if _, _, err := store.Open(ctx, ".tmp-stale.md"); err == nil {
		t.Fatal("hidden temp file open was accepted")
	}
	if err := store.Remove(ctx, "link.md"); err == nil {
		t.Fatal("export symlink removal was accepted")
	}
	removed, err := store.CleanIncomplete(ctx)
	if err != nil || removed != 1 {
		t.Fatalf("CleanIncomplete = %d, %v; want 1, nil", removed, err)
	}
	for _, preserved := range []string{keep, filepath.Join(root, "export-1.md"), filepath.Join(root, "link.md"), filepath.Join(root, ".tmp-link.md")} {
		if _, err = os.Lstat(preserved); err != nil {
			t.Fatalf("preserved path missing: %s (%v)", preserved, err)
		}
	}
	if _, err = os.Stat(filepath.Join(root, ".tmp-stale.md")); !os.IsNotExist(err) {
		t.Fatal("stale incomplete temp file was not removed")
	}
}
