package service

import (
	"context"
	"sort"
	"time"

	"github.com/rs/zerolog"

	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/repository"
)

func validateSchedule(interval int, rules []repository.IntervalRule, periods []repository.TimeWindow) error {
	if interval < 30 || interval > 86400 {
		return InvalidInput("采集间隔必须为 30～86400 秒")
	}
	periods = append([]repository.TimeWindow(nil), periods...)
	for _, rule := range rules {
		if rule.IntervalSeconds < 30 || rule.IntervalSeconds > 86400 {
			return InvalidInput("覆盖规则的间隔必须为 30～86400 秒")
		}
		periods = append(periods, rule.TimeWindow)
	}
	for _, period := range periods {
		start, err1 := time.Parse("15:04", period.Start)
		end, err2 := time.Parse("15:04", period.End)
		if err1 != nil || err2 != nil || start.Format("15:04") != period.Start || end.Format("15:04") != period.End {
			return InvalidInput("时段必须使用 HH:MM（00:00～23:59）")
		}
		if len(period.Weekdays) == 0 {
			return InvalidInput("每个时段至少选择一个星期")
		}
		for _, day := range period.Weekdays {
			if day < 1 || day > 7 {
				return InvalidInput("星期必须为 1～7（周一至周日）")
			}
		}
	}
	return nil
}

type timeRange struct{ start, end time.Time }

// 规则归属开始日；起止相同表示从当天该时刻到次日的整天。
func windowOnDay(window repository.TimeWindow, day time.Time) (timeRange, bool) {
	weekday := (int(day.Weekday())+6)%7 + 1
	found := false
	for _, d := range window.Weekdays {
		if d == weekday {
			found = true
			break
		}
	}
	if !found {
		return timeRange{}, false
	}
	start, err := time.Parse("15:04", window.Start)
	if err != nil {
		return timeRange{}, false
	}
	end, err := time.Parse("15:04", window.End)
	if err != nil {
		return timeRange{}, false
	}
	a := time.Date(day.Year(), day.Month(), day.Day(), start.Hour(), start.Minute(), 0, 0, day.Location())
	b := time.Date(day.Year(), day.Month(), day.Day(), end.Hour(), end.Minute(), 0, 0, day.Location())
	if window.End <= window.Start {
		b = time.Date(day.Year(), day.Month(), day.Day()+1, end.Hour(), end.Minute(), 0, 0, day.Location())
	}
	return timeRange{a, b}, true
}

func contains(window repository.TimeWindow, now time.Time) bool {
	for _, day := range []time.Time{now.AddDate(0, 0, -1), now} {
		r, ok := windowOnDay(window, day)
		if ok && !now.Before(r.start) && now.Before(r.end) {
			return true
		}
	}
	return false
}

func intervalAt(watch repository.Watch, now time.Time) time.Duration {
	for _, rule := range watch.IntervalRules {
		if contains(rule.TimeWindow, now) {
			return time.Duration(rule.IntervalSeconds) * time.Second
		}
	}
	return time.Duration(watch.IntervalSeconds) * time.Second
}

// 合并相交或首尾相接的时段。覆盖下一整周仍无出口即为每周全天免拉取，返回 nil 结束时间。
func noFetchAt(periods []repository.TimeWindow, now time.Time) (bool, *time.Time) {
	ranges := []timeRange{}
	for offset := -1; offset <= 8; offset++ {
		for _, period := range periods {
			if r, ok := windowOnDay(period, now.AddDate(0, 0, offset)); ok {
				ranges = append(ranges, r)
			}
		}
	}
	sort.Slice(ranges, func(i, j int) bool { return ranges[i].start.Before(ranges[j].start) })
	var end time.Time
	active := false
	for _, r := range ranges {
		if !active {
			if !now.Before(r.start) && now.Before(r.end) {
				active, end = true, r.end
			}
		} else {
			if r.start.After(end) {
				break
			}
			if r.end.After(end) {
				end = r.end
			}
		}
	}
	if !active {
		return false, nil
	}
	if !end.Before(now.AddDate(0, 0, 7)) {
		return true, nil
	}
	end = end.UTC()
	return true, &end
}

func (m *Monitoring) describeWatch(watch *repository.Watch, now time.Time) {
	watch.NoFetch, watch.NoFetchUntil = noFetchAt(watch.NoFetchPeriods, now.In(m.location))
}

func (m *Monitoring) nextRun(watch repository.Watch, now time.Time) *time.Time {
	// 手动运行不会打断已安排的免拉取跳过，避免同一连续区间重复写跳过记录。
	if active, until := noFetchAt(watch.NoFetchPeriods, now.In(m.location)); active {
		if until == nil && watch.NextRunAt == nil || until != nil && watch.NextRunAt != nil && until.Equal(*watch.NextRunAt) {
			return until
		}
	}
	next := now.Add(intervalAt(watch, now.In(m.location))).UTC()
	return &next
}

// 启动时显式接入；测试可调用 Tick 指定时间，不需要等真实的采集间隔。
func (m *Monitoring) StartScheduler() {
	m.notifications.Start()
	m.bot.Start()
	if !m.enabled {
		return
	}
	m.schedulerOnce.Do(func() {
		m.scheduler.Add(1)
		go func() {
			defer m.scheduler.Done()
			background, cancel := context.WithCancel(m.log.WithContext(context.Background()))
			stop := context.AfterFunc(m.ctx, cancel)
			defer cancel()
			defer stop()
			ticker := time.NewTicker(time.Second)
			defer ticker.Stop()
			for {
				select {
				case <-m.ctx.Done():
					return
				case now := <-ticker.C:
					ctx, span := logging.Start(background, "service.scheduler_tick")
					err := m.renewal.Expire(ctx, now)
					if err == nil {
						err = m.Tick(ctx, now)
					}
					if err != nil {
						logging.Error(ctx, err, "Scheduler tick failed", zerolog.ErrorLevel)
					}
					span.End(&err)
				}
			}
		}()
	})
}

// 每次选取最早到期的监控。离线多久都只运行一次，后续从当前时间重新计时。
func (m *Monitoring) Tick(ctx context.Context, now time.Time) (err error) {
	ctx, span := logging.Start(ctx, "service.schedule_due_watch")
	defer span.End(&err)
	if !m.enabled {
		return nil
	}
	if !m.work.TryLock() {
		return nil
	}
	if m.ctx.Err() != nil {
		m.work.Unlock()
		return nil
	}
	started := false
	defer func() {
		if !started {
			m.work.Unlock()
		}
	}()
	watches, err := m.store.Watches(ctx)
	if err != nil {
		return err
	}
	dueGaps, err := m.dueGapWatches(ctx, now)
	if err != nil {
		return err
	}
	account, err := m.store.Account(ctx)
	if err != nil {
		return err
	}
	if account.Status != "valid" {
		return nil
	}
	sort.Slice(watches, func(i, j int) bool {
		a, b := watches[i].NextRunAt, watches[j].NextRunAt
		if a == nil && b == nil || a != nil && b != nil && a.Equal(*b) {
			return watches[i].ID < watches[j].ID
		}
		return a == nil || b != nil && a.Before(*b)
	})
	for _, watch := range watches {
		if watch.Paused || watch.State != "ready" {
			continue
		}
		source := "automatic"
		if watch.NextRunAt != nil && watch.NextRunAt.After(now) {
			if !dueGaps[watch.ID] {
				continue
			}
			if active, _ := noFetchAt(watch.NoFetchPeriods, now.In(m.location)); active {
				continue
			}
			source = "gap_recovery"
		}
		// 全天免拉取且已记录过跳过时，不每秒制造一条相同记录；修改配置会重新设置 next_run_at。
		if active, until := noFetchAt(watch.NoFetchPeriods, now.In(m.location)); active && until == nil && watch.NextRunAt == nil {
			continue
		}
		_, started, err = m.startRunLocked(ctx, watch, source, now)
		return err
	}
	return nil
}
