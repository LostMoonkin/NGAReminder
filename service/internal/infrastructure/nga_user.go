package infrastructure

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"strings"
	"time"

	"github.com/rs/zerolog"
	"ngareminder/service/internal/logging"
)

type UserCandidate struct{ TID, PID, Timestamp int64 }
type UserPage struct {
	Candidates []UserCandidate
	HasMore    bool
	HasTotal   bool
}

func (n *NGA) UserPage(ctx context.Context, credentials Credentials, uid int64, replies bool, page int) (result UserPage, err error) {
	ctx, span := logging.Start(ctx, "infrastructure.nga.user_page")
	defer span.End(&err)
	zerolog.Ctx(ctx).Info().Int64("uid", uid).Bool("replies", replies).Int("page", page).Msg("Fetching NGA user list")
	body, err := n.userRequest(ctx, credentials, fmt.Sprint(uid), replies, page)
	if err != nil || body == nil {
		return result, err
	}
	return parseUserPage(body, uid, replies, page)
}

func (n *NGA) userRequest(ctx context.Context, credentials Credentials, uid string, replies bool, page int) (body []byte, err error) {
	query := url.Values{"authorid": {uid}, "__output": {"12"}, "page": {fmt.Sprint(page)}}
	if replies {
		query.Set("searchpost", "1")
	}
	// 跨用户搜索同样只发送协议要求的三个 Cookie 字段。
	parts := []string{}
	for _, part := range strings.Split(credentials.Cookie, ";") {
		name, _, _ := strings.Cut(strings.TrimSpace(part), "=")
		if name == "ngaPassportUid" || name == "ngaPassportCid" || name == "ngaPassportUrlencodedUname" {
			parts = append(parts, strings.TrimSpace(part))
		}
	}
	for attempt := 1; attempt <= 10; attempt++ {
		body, err = n.request(ctx, http.MethodGet, "/thread.php?"+query.Encode(), "", strings.Join(parts, "; "))
		// 后续回复页交给采集器结合已知总数判断是否结束；第一页始终重试并在失败时拒绝基线。
		if errors.Is(err, ErrNGASearchUnavailable) && replies && page > 1 {
			return nil, err
		}
		limit, delay := 10, time.Second
		if errors.Is(err, ErrNGASearchUnavailable) {
			limit, delay = 3, 2*time.Second
		}
		if err == nil || (!errors.Is(err, ErrNGABusy) && !errors.Is(err, ErrNGASearchUnavailable)) || attempt >= limit {
			return body, err
		}
		logging.Error(ctx, err, "NGA user request will be retried", zerolog.WarnLevel)
		zerolog.Ctx(ctx).Info().Int("attempt", attempt).Int("page", page).Bool("replies", replies).Msg("Retrying user search")
		if err = wait(ctx, delay); err != nil {
			return nil, err
		}
	}
	return body, err
}

func parseUserPage(body []byte, uid int64, replies bool, page int) (result UserPage, err error) {
	var raw struct {
		Result *struct {
			Items     *[]json.RawMessage `json:"__T"`
			Rows      *number            `json:"__ROWS"`
			TopicSize *number            `json:"__T__ROWS_PAGE"`
			ReplySize *number            `json:"__R__ROWS_PAGE"`
		} `json:"result"`
	}
	if err = json.Unmarshal(body, &raw); err != nil {
		return result, logging.Wrap(err, "decode NGA user list")
	}
	if raw.Result == nil || raw.Result.Items == nil {
		return result, logging.WithStack(errors.New("NGA user list is missing its result array"))
	}
	size := raw.Result.TopicSize
	if replies {
		size = raw.Result.ReplySize
	}
	if size == nil || *size <= 0 || raw.Result.Rows != nil && *raw.Result.Rows < 0 || !replies && raw.Result.Rows == nil {
		return result, logging.WithStack(errors.New("NGA user list has invalid pagination metadata"))
	}
	result.HasTotal = raw.Result.Rows != nil
	items := *raw.Result.Items
	if raw.Result.Rows != nil {
		pages := int64(*raw.Result.Rows) / int64(*size)
		if *raw.Result.Rows%*size != 0 {
			pages++
		}
		result.HasMore = int64(page) < pages
	} else {
		result.HasMore = len(items) >= int(*size)
	}
	for _, item := range items {
		var fields map[string]json.RawMessage
		if err = json.Unmarshal(item, &fields); err != nil {
			return result, logging.Wrap(err, "decode NGA user list item")
		}
		if string(fields["denied"]) == "true" {
			continue
		}
		if replies {
			if len(fields["__P"]) == 0 || string(fields["__P"]) == "null" {
				continue
			}
			reply := fields["__P"]
			fields = nil
			if err = json.Unmarshal(reply, &fields); err != nil {
				return result, logging.Wrap(err, "decode NGA reply candidate")
			}
		}
		// 占位回复可能用空字符串代替时间或 ID；跳过占位，而不是用零值建立水位。
		read := func(key string) int64 {
			var value number
			if value.UnmarshalJSON(fields[key]) != nil {
				return 0
			}
			return int64(value)
		}
		tid, pid, author, timestamp := read("tid"), read("pid"), read("authorid"), read("postdate")
		if tid <= 0 || timestamp <= 0 || author != uid || replies && pid <= 0 {
			continue
		}
		result.Candidates = append(result.Candidates, UserCandidate{tid, pid, timestamp})
	}
	return result, nil
}

func (n *NGA) PostByPID(ctx context.Context, credentials Credentials, tid, pid int64) (post ParsedPost, err error) {
	ctx, span := logging.Start(ctx, "infrastructure.nga.post_by_pid")
	defer span.End(&err)
	zerolog.Ctx(ctx).Info().Int64("tid", tid).Int64("pid", pid).Msg("Fetching NGA reply detail")
	form := url.Values{"tid": {fmt.Sprint(tid)}, "pid": {fmt.Sprint(pid)}}
	body, err := n.request(ctx, http.MethodPost, "/app_api.php?__lib=post&__act=list", form.Encode(), credentials.Cookie)
	if err != nil {
		return post, err
	}
	return parsePostByPID(body, tid, pid)
}

func parsePostByPID(body []byte, tid, pid int64) (post ParsedPost, err error) {
	var raw struct {
		Posts  []rawPost `json:"result"`
		Prefix string    `json:"attachPrefix"`
		Title  string    `json:"tsubject"`
	}
	if err = json.Unmarshal(body, &raw); err != nil {
		return post, logging.Wrap(err, "decode NGA reply detail")
	}
	if pid <= 0 || len(raw.Posts) != 1 || int64(raw.Posts[0].PID) != pid {
		return post, logging.WithStack(errors.New("NGA reply detail does not match the requested PID"))
	}
	raw.Posts[0].Comments = nil
	posts, err := parsePost(raw.Posts[0], tid, nil, raw.Prefix)
	if err != nil {
		return post, err
	}
	post = posts[0]
	// 按 PID 查询时 lou=0 不代表主楼；身份由请求确定，不能覆盖主题或其他回复。
	post.Kind, post.Key = "reply", fmt.Sprintf("pid:%d", pid)
	post.SourceURL = fmt.Sprintf("%s/read.php?tid=%d&pid=%d", ngaBaseURL, tid, pid)
	if post.Subject == "" {
		post.Subject = raw.Title
	}
	return post, nil
}
