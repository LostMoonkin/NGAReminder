package infrastructure

import (
	"context"
	"sync"

	"ngareminder/service/internal/logging"
)

// 调用方按配置切成小批次；只并发请求，返回结果仍按页号排列。
func (n *NGA) ThreadPages(ctx context.Context, credentials Credentials, tid int64, first, last int) (pages []ThreadPage, err error) {
	ctx, span := logging.Start(ctx, "infrastructure.nga.thread_pages")
	defer span.End(&err)
	pages = make([]ThreadPage, last-first+1)
	if first == last {
		pages[0], err = n.ThreadPage(ctx, credentials, tid, first)
		return pages, err
	}
	batch, cancel := context.WithCancelCause(ctx)
	defer cancel(nil)
	var workers sync.WaitGroup
	for i := range pages {
		workers.Add(1)
		go func() {
			defer workers.Done()
			defer func() {
				if value := recover(); value != nil {
					cancel(logging.FromPanic(value))
				}
			}()
			page, e := n.ThreadPage(batch, credentials, tid, first+i)
			if e != nil {
				cancel(e)
				return
			}
			pages[i] = page
		}()
	}
	workers.Wait()
	return pages, logging.Wrap(context.Cause(batch), "fetch NGA page batch")
}
