package content

import (
	"encoding/json"
	"os"
	"reflect"
	"testing"
)

// 预期值由归档 Rust 的 markup.rs 和 sender.rs 原函数执行生成，避免用 Go 实现定义自身行为。
func TestFeishuMatchesRust(t *testing.T) {
	raw, err := os.ReadFile("testdata/feishu_rust.json")
	if err != nil {
		t.Fatal(err)
	}
	var fixtures []struct {
		Name, Body, Text string
		Images           []string
	}
	if err := json.Unmarshal(raw, &fixtures); err != nil {
		t.Fatal(err)
	}
	for _, fixture := range fixtures {
		t.Run(fixture.Name, func(t *testing.T) {
			text, images := Feishu(fixture.Body)
			if text != fixture.Text || !reflect.DeepEqual(images, fixture.Images) {
				t.Fatalf("Rust parity mismatch:\ntext: got %q, want %q\nimages: got %q, want %q", text, fixture.Text, images, fixture.Images)
			}
		})
	}
}
