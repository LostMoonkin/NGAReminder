package service

import (
	"ngareminder/service/internal/repository"
	"testing"
	"time"
)

func TestGapAttemptScheduleAndBaseline(t *testing.T) {
	start := time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)
	gap := repository.FloorGap{Status: "pending", FirstSeen: start, Deadline: start.Add(120 * time.Minute)}
	for index, minutes := range []int{5, 15, 35, 65, 95, 120} {
		if got := gapDue(gap, start.Add(time.Duration(minutes)*time.Minute)); got != index {
			t.Fatal("incorrect recovery round", minutes, got)
		}
		gap.NextAttempt = index + 1
	}
	if got := gapDue(gap, start.Add(121*time.Minute)); got != -1 {
		t.Fatal("recovery exceeded its deadline")
	}
	watch := repository.Watch{ID: 1, CursorFloor: 100, BaselineComplete: true}
	gaps := newFloorGaps(watch, map[int64]int{102: 6, 104: 7}, 104, start)
	if len(gaps) != 2 || gaps[0].Floor != 101 || gaps[1].Floor != 103 || gaps[0].PageHint != 6 || gaps[1].PageHint != 7 {
		t.Fatal("incorrect incremental floor gap range", gaps)
	}
	watch.BaselineComplete = false
	if len(newFloorGaps(watch, nil, 104, start)) != 0 {
		t.Fatal("baseline holes became recovery gaps")
	}
}
