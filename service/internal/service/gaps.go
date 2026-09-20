package service

import (
	"context"
	"github.com/rs/zerolog"
	"ngareminder/service/internal/infrastructure"
	"ngareminder/service/internal/repository"
	"slices"
	"time"
)

var gapMinutes = []int{5, 15, 35, 65, 95, 120}

func gapDue(gap repository.FloorGap, now time.Time) int {
	if gap.Status != "pending" || now.After(gap.Deadline) {
		return -1
	}
	latest := -1
	for i := gap.NextAttempt; i < len(gapMinutes); i++ {
		due := gap.FirstSeen.Add(time.Duration(gapMinutes[i]) * time.Minute)
		// 最后一轮预留一个调度 tick，确保启动时间不晚于严格截止时间。
		if i == len(gapMinutes)-1 {
			due = due.Add(-time.Second)
		}
		if !now.Before(due) {
			latest = i
		}
	}
	return latest
}
func newFloorGaps(watch repository.Watch, seen map[int64]int, maxFloor int64, now time.Time) []repository.FloorGap {
	result := []repository.FloorGap{}
	if !watch.BaselineComplete {
		return result
	}
	pageHint := 0
	for floor := maxFloor; floor > watch.CursorFloor; floor-- {
		if page, ok := seen[floor]; ok {
			pageHint = page
		} else {
			result = append(result, repository.FloorGap{WatchID: watch.ID, Floor: floor, PageHint: pageHint, Status: "pending", FirstSeen: now.UTC(), Deadline: now.Add(120 * time.Minute).UTC()})
		}
	}
	slices.Reverse(result)
	return result
}
func loadGapMap(ctx context.Context, store *repository.Store, watchID int64) (map[int64]repository.FloorGap, error) {
	gaps, err := store.FloorGaps(ctx, watchID)
	if err != nil {
		return nil, err
	}
	byFloor := map[int64]repository.FloorGap{}
	for _, gap := range gaps {
		if gap.Status != "resolved" {
			byFloor[gap.Floor] = gap
		}
	}
	return byFloor, nil
}
func (m *Monitoring) recordThreadPosts(ctx context.Context, tx *repository.Store, watch repository.Watch, posts []repository.Post, silent bool, gaps map[int64]repository.FloorGap) error {
	if err := m.notifications.Record(ctx, tx, watch, posts, silent); err != nil {
		return err
	}
	for _, post := range posts {
		if post.Kind == "comment" {
			continue
		}
		if gap, ok := gaps[post.Floor]; ok {
			gap.Status = "resolved"
			if err := tx.SaveFloorGap(ctx, &gap); err != nil {
				return err
			}
			delete(gaps, post.Floor)
			zerolog.Ctx(ctx).Info().Int64("watch_id", watch.ID).Int64("floor", post.Floor).Msg("Recorded floor gap recovered")
		}
	}
	return nil
}
func advanceGapAttempts(ctx context.Context, tx *repository.Store, watchID int64, now time.Time) ([]repository.FloorGap, error) {
	gaps, err := tx.FloorGaps(ctx, watchID)
	if err != nil {
		return nil, err
	}
	var due []repository.FloorGap
	for _, gap := range gaps {
		if index := gapDue(gap, now); index >= 0 {
			gap.NextAttempt = index + 1
			zerolog.Ctx(ctx).Info().Int64("watch_id", watchID).Int64("floor", gap.Floor).Int("attempt", gap.NextAttempt).Time("deadline", gap.Deadline).Msg("Advancing floor recovery attempt")
			if err = tx.SaveFloorGap(ctx, &gap); err != nil {
				return nil, err
			}
			due = append(due, gap)
		}
	}
	return due, nil
}
func expireCompletedGaps(ctx context.Context, tx *repository.Store, watchID int64) error {
	gaps, err := tx.FloorGaps(ctx, watchID)
	if err != nil {
		return err
	}
	for _, gap := range gaps {
		if gap.Status == "pending" && gap.NextAttempt >= len(gapMinutes) {
			gap.Status = "expired"
			if err = tx.SaveFloorGap(ctx, &gap); err != nil {
				return err
			}
		}
	}
	return nil
}
func (m *Monitoring) scheduledGaps(ctx context.Context, now time.Time) (due map[int64]bool, expired []repository.FloorGap, err error) {
	gaps, err := m.store.FloorGaps(ctx, 0)
	if err != nil {
		return nil, nil, err
	}
	due = map[int64]bool{}
	for _, gap := range gaps {
		if gap.Status != "pending" {
			continue
		}
		if now.After(gap.Deadline) || gap.NextAttempt >= len(gapMinutes) {
			expired = append(expired, gap)
			continue
		}
		if gapDue(gap, now) >= 0 {
			due[gap.WatchID] = true
		}
	}
	return due, expired, nil
}

// 缺口只访问发现时的提示页及相邻页；旧库没有提示时按楼层与每页条数估算。
func gapRecoveryPages(due []repository.FloorGap, first infrastructure.ThreadPage) []int {
	pages := map[int]bool{}
	for _, gap := range due {
		hint := gap.PageHint
		if hint < 1 {
			hint = threadFloorPage(gap.Floor, first)
		}
		hint = min(hint, first.TotalPages)
		for page := max(1, hint-1); page <= min(first.TotalPages, hint+1); page++ {
			pages[page] = true
		}
	}
	result := make([]int, 0, len(pages))
	for page := range pages {
		result = append(result, page)
	}
	slices.Sort(result)
	return result
}

func (m *Monitoring) collectGaps(ctx context.Context, credentials infrastructure.Credentials, watch *repository.Watch, run *repository.Run, recovery []repository.FloorGap) error {
	gaps, err := loadGapMap(ctx, m.store, watch.ID)
	if err != nil {
		return err
	}
	first, err := m.nga.ThreadPage(ctx, credentials, watch.TID, 1)
	if err != nil {
		return err
	}
	run.Pages = 1
	if err := m.collectGapPages(ctx, credentials, watch, run, gaps, recovery, first, nil); err != nil {
		return err
	}
	now := time.Now().UTC()
	run.Status, run.FinishedAt = "success", &now
	return m.store.Transaction(ctx, func(ctx context.Context, tx *repository.Store) error {
		if err := expireCompletedGaps(ctx, tx, watch.ID); err != nil {
			return err
		}
		return tx.SaveRun(ctx, run)
	})
}

func (m *Monitoring) collectGapPages(ctx context.Context, credentials infrastructure.Credentials, watch *repository.Watch, run *repository.Run, gaps map[int64]repository.FloorGap, recovery []repository.FloorGap, first infrastructure.ThreadPage, fetched map[int]bool) error {
	for _, page := range gapRecoveryPages(recovery, first) {
		if fetched[page] {
			continue
		}
		data := first
		var err error
		if page > 1 {
			data, err = m.nga.ThreadPage(ctx, credentials, watch.TID, page)
			if err != nil {
				return err
			}
		}
		// 同一个主题同一轮只抓一遍；只补已有缺口及其评论，不推进实时水位。
		posts := []repository.Post{}
		for _, post := range data.Posts {
			floor := post.Floor
			if post.Kind == "comment" {
				floor = post.ParentFloor
			}
			if _, ok := gaps[floor]; ok {
				posts = append(posts, m.storedPost(post))
			}
		}
		next := *run
		err = m.store.Transaction(ctx, func(ctx context.Context, tx *repository.Store) error {
			saved, e := tx.InsertPosts(ctx, posts)
			if e != nil {
				return e
			}
			if e = m.recordThreadPosts(ctx, tx, *watch, posts, false, gaps); e != nil {
				return e
			}
			if page > 1 {
				next.Pages++
			}
			next.Saved += saved
			return tx.SaveRun(ctx, &next)
		})
		if err != nil {
			return err
		}
		*run = next
		m.resources.Collect(ctx, posts)
	}
	return nil
}
