package infrastructure

import (
	"context"
	"io"
	"os"
	"path/filepath"
	"testing"
)

func TestNestedAssetPaths(t *testing.T) {
	dir := t.TempDir()
	if err := os.Mkdir(filepath.Join(dir, "ab"), 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "ab", "fixture.png"), []byte("fixture"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink("ab", filepath.Join(dir, "link")); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink("fixture.png", filepath.Join(dir, "ab", "link.png")); err != nil {
		t.Fatal(err)
	}
	assets := Assets{Path: dir}
	ctx := context.Background()
	file, err := assets.Open(ctx, "ab/fixture.png")
	if err != nil {
		t.Fatal(err)
	}
	raw, err := io.ReadAll(file)
	_ = file.Close()
	if err != nil || string(raw) != "fixture" {
		t.Fatal("nested resource was not read", err)
	}
	for _, name := range []string{"", "/ab/fixture.png", "../ab/fixture.png", "ab/../ab/fixture.png", "ab//fixture.png", "ab\\fixture.png", ".secret", "link/fixture.png", "ab/link.png", "ab"} {
		file, err = assets.Open(ctx, name)
		if err == nil {
			_ = file.Close()
			t.Fatalf("unsafe asset path was accepted: %s", name)
		}
	}
}
