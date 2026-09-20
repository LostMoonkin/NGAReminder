package infrastructure

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"strconv"
	"strings"
	"testing"
)

func TestFeishuImageDownloadMatchesRust(t *testing.T) {
	for _, tc := range []struct {
		name, mime, body string
		length           int64
		wantOK           bool
	}{
		{"image", "image/png; charset=binary", "image fixture", -1, true},
		{"missing_type", "", "image fixture", -1, false},
		{"wrong_type", "text/html", "image fixture", -1, false},
		{"empty", "image/png", "", 0, false},
		{"declared_oversize", "image/png", "small", 33, false},
		{"streamed_oversize", "image/png", strings.Repeat("x", 33), -1, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			n := NewNotifier(roundTrip(func(r *http.Request) (*http.Response, error) {
				return &http.Response{StatusCode: 200, Header: http.Header{"Content-Type": {tc.mime}}, Body: io.NopCloser(strings.NewReader(tc.body)), ContentLength: tc.length, Request: r}, nil
			}))
			defer n.Close()
			data, mime, err := n.DownloadResource(context.Background(), "https://img.nga.cn/fixture.png", 32, true)
			if (err == nil) != tc.wantOK {
				t.Fatalf("image download result: %v, want success=%v", err, tc.wantOK)
			}
			if tc.wantOK && (string(data) != tc.body || mime != "image/png") {
				t.Fatalf("image MIME or bytes changed: mime=%q data=%q", mime, data)
			}
		})
	}
}

func TestFeishuImageRedirectsMatchRust(t *testing.T) {
	for _, tc := range []struct {
		name, destination string
		redirects, calls  int
		wantOK            bool
	}{
		{"trusted_chain", "https://img4.nga.178.com", 4, 5, true},
		{"too_many_hops", "https://img.nga.cn", 5, 5, false},
		{"untrusted_host", "https://untrusted.invalid", 1, 1, false},
		{"http_downgrade", "http://img.nga.cn", 1, 1, false},
		{"resource_only_host", "https://img6.nga.cn", 1, 1, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			calls := 0
			n := NewNotifier(roundTrip(func(r *http.Request) (*http.Response, error) {
				calls++
				hop, _ := strconv.Atoi(strings.TrimPrefix(r.URL.Path, "/"))
				response := &http.Response{StatusCode: 200, Header: http.Header{"Content-Type": {"image/png"}}, Body: io.NopCloser(strings.NewReader("fixture")), Request: r}
				if hop < tc.redirects {
					response.StatusCode = http.StatusFound
					response.Header.Set("Location", fmt.Sprintf("%s/%d", tc.destination, hop+1))
				}
				return response, nil
			}))
			defer n.Close()
			_, _, err := n.DownloadResource(context.Background(), "https://img.nga.cn/0", 32, true)
			if (err == nil) != tc.wantOK || calls != tc.calls {
				t.Fatalf("redirect policy mismatch: error=%v calls=%d, want success=%v calls=%d", err, calls, tc.wantOK, tc.calls)
			}
		})
	}
}
