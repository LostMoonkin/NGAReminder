package repository

import (
	"context"
	"path/filepath"
	"testing"
)

func TestResourceStatePersists(t *testing.T) {
	ctx := context.Background()
	path := filepath.Join(t.TempDir(), "resources.db")
	store, err := Open(ctx, path)
	if err != nil {
		t.Fatal(err)
	}
	items := []Resource{
		{URL: "https://img.nga.cn/missing.png", HTTPStatus: 404, Error: "not found"},
		{URL: "https://img.nga.cn/ignored.png", Ignored: true},
	}
	for i := range items {
		if err = store.SaveResource(ctx, &items[i]); err != nil {
			t.Fatal(err)
		}
	}
	if err = store.Close(ctx); err != nil {
		t.Fatal(err)
	}
	store, err = Open(ctx, path)
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close(ctx)
	missing, err := store.Resource(ctx, items[0].URL)
	if err != nil || missing.HTTPStatus != 404 || missing.Ignored {
		t.Fatalf("404 state did not persist: %+v %v", missing, err)
	}
	ignored, err := store.Resource(ctx, items[1].URL)
	if err != nil || !ignored.Ignored || ignored.HTTPStatus != 0 {
		t.Fatalf("ignored state did not persist: %+v %v", ignored, err)
	}
}
