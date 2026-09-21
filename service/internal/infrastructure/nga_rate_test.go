package infrastructure

import (
	"context"
	"errors"
	"io"
	"net/http"
	"strings"
	"sync"
	"testing"
	"testing/synctest"
	"time"
)

func TestNGAProductionTimingDefaults(t *testing.T) {
	n := NewNGA("fixture", nil)
	defer n.Close()
	if n.interval != 500*time.Millisecond || n.busyRetryDelay != 3*time.Second || n.searchRetryDelay != 2*time.Second {
		t.Fatalf("production NGA timing changed: interval=%s busy_retry=%s search_retry=%s", n.interval, n.busyRetryDelay, n.searchRetryDelay)
	}
	fixture := NewNGA("fixture", roundTrip(func(*http.Request) (*http.Response, error) { return nil, errors.New("unused") }))
	defer fixture.Close()
	if fixture.interval != 0 || fixture.busyRetryDelay != 0 || fixture.searchRetryDelay != 0 {
		t.Fatalf("fixture NGA uses wall-clock timing: interval=%s busy_retry=%s search_retry=%s", fixture.interval, fixture.busyRetryDelay, fixture.searchRetryDelay)
	}
}

func TestNGARateLimitAllowsOverlappingResponses(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		var mu sync.Mutex
		var starts []time.Time
		active, peak := 0, 0
		n := NewNGA("fixture", roundTrip(func(r *http.Request) (*http.Response, error) {
			mu.Lock()
			starts = append(starts, time.Now())
			active++
			peak = max(peak, active)
			mu.Unlock()
			defer func() { mu.Lock(); active--; mu.Unlock() }()
			time.Sleep(2 * time.Second)
			return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(`{"code":0}`)), Header: make(http.Header)}, nil
		}))
		n.interval = 500 * time.Millisecond
		defer n.Close()
		var workers sync.WaitGroup
		for range 121 {
			workers.Add(1)
			go func() {
				defer workers.Done()
				if _, err := n.request(context.Background(), "GET", "/fixture", "", "fixture-cookie"); err != nil {
					t.Error(err)
				}
			}()
		}
		workers.Wait()
		if len(starts) != 121 || peak < 2 || starts[120].Sub(starts[0]) != time.Minute {
			t.Fatalf("unexpected global request pacing: count=%d peak=%d", len(starts), peak)
		}
		for i := 1; i < len(starts); i++ {
			if starts[i].Sub(starts[i-1]) < 500*time.Millisecond {
				t.Fatal("requests exceeded 120 QPM")
			}
		}
	})
}

func TestNGARateWaitCancellation(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		calls := 0
		n := NewNGA("fixture", roundTrip(func(r *http.Request) (*http.Response, error) {
			calls++
			return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(`{"code":0}`)), Header: make(http.Header)}, nil
		}))
		n.interval = 500 * time.Millisecond
		defer n.Close()
		if _, err := n.request(context.Background(), "GET", "/fixture", "", ""); err != nil {
			t.Fatal(err)
		}
		ctx, cancel := context.WithCancel(context.Background())
		done := make(chan error, 1)
		go func() { _, err := n.request(ctx, "GET", "/fixture", "", ""); done <- err }()
		synctest.Wait()
		cancel()
		if err := <-done; !errors.Is(err, context.Canceled) || calls != 1 {
			t.Fatal("cancelled rate wait sent an HTTP request", err)
		}
	})
}
