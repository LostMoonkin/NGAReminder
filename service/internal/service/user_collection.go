package service

import (
	"context"
	"errors"

	"github.com/rs/zerolog"
	"ngareminder/service/internal/infrastructure"
	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/repository"
)

func newer(a, b repository.UserCursor) bool {
	return a.Timestamp > b.Timestamp || a.Timestamp == b.Timestamp && a.ID > b.ID
}

func (m *Monitoring) userCandidates(ctx context.Context, credentials infrastructure.Credentials, watch repository.Watch, run *repository.Run, replies bool) (candidates []infrastructure.UserCandidate, cursor repository.UserCursor, err error) {
	cursor = watch.TopicCursor
	if replies {
		cursor = watch.ReplyCursor
	}
	original := cursor
	var previous int64
	hasTotal := false
	for page := 1; ; page++ {
		result, e := m.nga.UserPage(ctx, credentials, watch.UID, replies, page)
		if e != nil {
			if replies && page > 1 && !hasTotal && errors.Is(e, infrastructure.ErrNGASearchUnavailable) {
				logging.Error(ctx, e, "NGA reply pagination ended after a successful page with no total count", zerolog.InfoLevel)
				run.Pages++
				break
			}
			return nil, cursor, e
		}
		hasTotal = hasTotal || result.HasTotal
		run.Pages++
		reachedOld := false
		for _, candidate := range result.Candidates {
			if replies && previous != 0 && candidate.Timestamp > previous {
				return nil, cursor, logging.WithStack(errors.New("NGA reply list is not ordered by descending publication time"))
			}
			previous = candidate.Timestamp
			id := candidate.TID
			if replies {
				id = candidate.PID
			}
			point := repository.UserCursor{Timestamp: candidate.Timestamp, ID: id}
			if newer(point, cursor) {
				cursor = point
			}
			if candidate.Timestamp < original.Timestamp {
				reachedOld = true
			}
			if watch.BaselineComplete && newer(point, original) {
				candidates = append(candidates, candidate)
			}
		}
		// 主题可能因旧帖的新回复重新排序，按服务端计数翻完；回复按发布时间倒序，只读到旧水位。
		// 初次回复基线只需第一个含有效内容的页面，无需遍历历史。
		if !result.HasMore || replies && (reachedOld || !watch.BaselineComplete && len(result.Candidates) > 0) {
			break
		}
	}
	zerolog.Ctx(ctx).Info().Int64("watch_id", watch.ID).Int64("uid", watch.UID).Bool("replies", replies).
		Int64("cursor_timestamp", cursor.Timestamp).Int64("cursor_id", cursor.ID).Int("candidates", len(candidates)).Msg("User list collected")
	return candidates, cursor, nil
}

func (m *Monitoring) collectUser(ctx context.Context, credentials infrastructure.Credentials, watch *repository.Watch, run *repository.Run) error {
	topics, topicCursor, err := m.userCandidates(ctx, credentials, *watch, run, false)
	if err != nil {
		return err
	}
	replies, replyCursor, err := m.userCandidates(ctx, credentials, *watch, run, true)
	if err != nil {
		return err
	}
	posts := []repository.Post{}
	for _, candidate := range topics {
		page, err := m.nga.ThreadPage(ctx, credentials, candidate.TID, 1)
		if err != nil {
			return err
		}
		run.Pages++
		found := false
		for _, post := range page.Posts {
			if post.Kind != "main" {
				continue
			}
			found = true
			if post.AuthorUID == watch.UID {
				posts = append(posts, storedPost(post))
			} else {
				zerolog.Ctx(ctx).Info().Int64("watch_id", watch.ID).Int64("tid", candidate.TID).Int64("expected_uid", watch.UID).Int64("author_uid", post.AuthorUID).Msg("Ignoring topic whose detail author differs from the watched user")
			}
			break
		}
		if !found {
			return logging.WithStack(errors.New("NGA topic detail is missing the main post"))
		}
	}
	for _, candidate := range replies {
		post, err := m.nga.PostByPID(ctx, credentials, candidate.TID, candidate.PID)
		if err != nil {
			return err
		}
		run.Pages++
		if post.AuthorUID != watch.UID {
			zerolog.Ctx(ctx).Info().Int64("watch_id", watch.ID).Int64("tid", candidate.TID).Int64("pid", candidate.PID).
				Int64("expected_uid", watch.UID).Int64("author_uid", post.AuthorUID).Msg("Ignoring reply whose detail author differs from the watched user")
			continue
		}
		posts = append(posts, storedPost(post))
	}
	// 两份列表和所有详情成功后才一次提交；任何失败都保留原水位和基线。
	watch.TopicCursor, watch.ReplyCursor = topicCursor, replyCursor
	return m.finishSuccessfulRun(ctx, watch, run, 0, posts, nil)
}

func storedPost(post infrastructure.ParsedPost) repository.Post {
	return repository.Post{TID: post.TID, Key: post.Key, PID: post.PID, Kind: post.Kind, Floor: post.Floor,
		ParentKey: post.ParentKey, ParentFloor: post.ParentFloor, CommentToID: post.CommentToID, AuthorUID: post.AuthorUID,
		Author: post.Author, Subject: post.Subject, Body: post.Body, PublishedAt: post.PublishedAt, SourceURL: post.SourceURL, Resources: post.Resources}
}
