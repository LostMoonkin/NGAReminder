package service

import (
	"testing"
	"time"

	"ngareminder/service/internal/repository"
)

func TestWeeklyScheduleBoundaries(t *testing.T) {
	location, err := time.LoadLocation("Asia/Shanghai")
	if err != nil {
		t.Fatal(err)
	}
	parse := func(text string) time.Time {
		value, err := time.ParseInLocation("2006-01-02 15:04:05", text, location)
		if err != nil {
			t.Fatal(err)
		}
		return value
	}
	overnight := repository.TimeWindow{Weekdays: []int{1}, Start: "23:00", End: "02:00"}
	watch := repository.Watch{IntervalSeconds: 60, IntervalRules: []repository.IntervalRule{
		{TimeWindow: overnight, IntervalSeconds: 300},
		{TimeWindow: repository.TimeWindow{Weekdays: []int{1, 2}, Start: "00:00", End: "00:00"}, IntervalSeconds: 120},
	}}
	periods := []repository.TimeWindow{overnight,
		{Weekdays: []int{2}, Start: "01:00", End: "03:00"},
		{Weekdays: []int{2}, Start: "03:00", End: "04:00"},
	}
	for _, c := range []struct {
		at       string
		interval int
		blocked  bool
	}{
		{"2026-09-14 22:59:59", 120, false},
		{"2026-09-14 23:00:00", 300, true},
		{"2026-09-15 01:59:59", 300, true},
		{"2026-09-15 02:00:00", 120, true},
		{"2026-09-15 03:00:00", 120, true},
		{"2026-09-15 04:00:00", 120, false},
		{"2026-09-16 00:00:00", 60, false},
		{"2026-09-21 23:30:00", 300, true},
	} {
		now := parse(c.at)
		if got := intervalAt(watch, now); got != time.Duration(c.interval)*time.Second {
			t.Errorf("%s: interval=%s", c.at, got)
		}
		blocked, end := noFetchAt(periods, now)
		if blocked != c.blocked {
			t.Fatalf("%s: blocked=%v", c.at, blocked)
		}
		if blocked && (end == nil || end.In(location).Format("15:04") != "04:00" || end.Location() != time.UTC) {
			t.Fatalf("contiguous windows did not merge: %v", end)
		}
	}
	full := []repository.TimeWindow{{Weekdays: []int{1, 2, 3, 4, 5, 6, 7}, Start: "00:00", End: "00:00"}}
	if active, end := noFetchAt(full, parse("2026-09-14 12:00:00")); !active || end != nil {
		t.Fatalf("full week should have no finite end: %v %v", active, end)
	}
	// 周日起始的跨午夜段也必须在周一延续。
	sunday := []repository.TimeWindow{{Weekdays: []int{7}, Start: "23:00", End: "01:00"}}
	if active, end := noFetchAt(sunday, parse("2026-09-14 00:00:00")); !active || end == nil || !end.Equal(parse("2026-09-14 01:00:00")) {
		t.Fatalf("week rollover failed: %v %v", active, end)
	}
	for _, invalid := range []repository.TimeWindow{
		{Weekdays: []int{1}, Start: "24:00", End: "02:00"},
		{Weekdays: []int{1}, Start: "9:00", End: "12:00"},
		{Weekdays: []int{0}, Start: "09:00", End: "12:00"},
		{Start: "09:00", End: "12:00"},
	} {
		if validateSchedule(60, nil, []repository.TimeWindow{invalid}) == nil {
			t.Errorf("accepted invalid window: %+v", invalid)
		}
	}
	for _, interval := range []int{0, 29, 86401} {
		if validateSchedule(interval, nil, nil) == nil {
			t.Errorf("accepted interval %d", interval)
		}
	}
	if validateSchedule(30, []repository.IntervalRule{{TimeWindow: overnight, IntervalSeconds: 86400}}, nil) != nil {
		t.Fatal("valid schedule bounds rejected")
	}
	if validateSchedule(60, []repository.IntervalRule{{TimeWindow: overnight, IntervalSeconds: 29}}, nil) == nil {
		t.Fatal("invalid override interval accepted")
	}
}
