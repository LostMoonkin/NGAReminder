package infrastructure

import (
	"encoding/json"
	"errors"
	"fmt"
	"html"
	"net/url"
	"regexp"
	"strconv"
	"strings"
	"time"

	"ngareminder/service/internal/logging"
)

// NGA 同一数字字段可能是 JSON number 或字符串；不把原值写入解析错误。
type number int64

func (n *number) UnmarshalJSON(raw []byte) error {
	value, err := strconv.ParseInt(strings.Trim(string(raw), "\""), 10, 64)
	if err != nil {
		return logging.WithStack(errors.New("NGA response contains an invalid numeric field"))
	}
	*n = number(value)
	return nil
}

type ThreadMetadata struct {
	TID        int64
	FID        number `json:"fid"`
	Title      string `json:"tsubject"`
	ForumName  string `json:"forum_name"`
	AuthorUID  number `json:"tauthorid"`
	AuthorName string `json:"tauthor"`
}

type ThreadPage struct {
	Metadata   ThreadMetadata
	Title      string
	Page       int
	TotalPages int
	PerPage    int
	Rows       int64
	Posts      []ParsedPost
}

type ParsedPost struct {
	ResourceNames                                                       map[string]string
	Thread                                                              ThreadMetadata
	PageNumber                                                          int
	RawPayload                                                          json.RawMessage
	TID, PID, Floor, ParentFloor, AuthorUID                             int64
	Key, Kind, ParentKey, CommentToID, Author, Subject, Body, SourceURL string
	PublishedAt                                                         *time.Time
	Resources                                                           []string
}

type rawPost struct {
	Raw         json.RawMessage `json:"-"`
	TID         number          `json:"tid"`
	PID         number          `json:"pid"`
	Floor       *number         `json:"lou"`
	Timestamp   *number         `json:"postdatetimestamp"`
	Postdate    string          `json:"postdate"`
	Subject     string          `json:"subject"`
	Content     string          `json:"content"`
	CommentToID json.RawMessage `json:"comment_to_id"`
	Author      struct {
		UID  *number `json:"uid"`
		Name string  `json:"username"`
	} `json:"author"`
	Comments    []rawPost `json:"comments"`
	Attachments []struct {
		OriginalName string `json:"url_utf8_org_name"`
		Name         string `json:"name"`
		URL          string `json:"attachurl"`
		Path         string `json:"path"`
		Thumb        string `json:"thumb"`
	} `json:"attches"`
}

func (p *rawPost) UnmarshalJSON(data []byte) error {
	type fields rawPost
	if err := json.Unmarshal(data, (*fields)(p)); err != nil {
		return err
	}
	p.Raw = append(json.RawMessage(nil), data...)
	return nil
}

func parseThreadPage(body []byte, tid int64, requestedPage int) (result ThreadPage, err error) {
	var raw struct {
		ThreadMetadata
		Page         number    `json:"currentPage"`
		Total        number    `json:"totalPage"`
		PerPage      number    `json:"perPage"`
		Rows         number    `json:"vrows"`
		AttachPrefix string    `json:"attachPrefix"`
		Posts        []rawPost `json:"result"`
	}
	if err = json.Unmarshal(body, &raw); err != nil {
		return result, logging.Wrap(err, "decode NGA thread")
	}
	if int(raw.Page) != requestedPage || raw.Page < 1 || raw.Total < raw.Page || raw.PerPage < 1 || raw.Rows < 1 || len(raw.Posts) == 0 {
		return result, logging.WithStack(errors.New("NGA thread page is empty or has inconsistent pagination"))
	}
	raw.ThreadMetadata.TID = tid
	result = ThreadPage{Metadata: raw.ThreadMetadata, Title: raw.Title, Page: int(raw.Page), TotalPages: int(raw.Total), PerPage: int(raw.PerPage), Rows: int64(raw.Rows)}
	for _, rawPost := range raw.Posts {
		posts, parseErr := parsePost(rawPost, tid, nil, raw.AttachPrefix)
		if parseErr != nil {
			return ThreadPage{}, parseErr
		}
		if posts[0].Kind == "main" && posts[0].Subject == "" {
			posts[0].Subject = raw.Title
		}
		for i := range posts {
			posts[i].PageNumber = int(raw.Page)
			posts[i].Thread = raw.ThreadMetadata
		}
		result.Posts = append(result.Posts, posts...)
	}
	// hot_post 是 result 的引用，不能再插入一遍；主楼以 main 键去重，PID 可为 0。
	return result, nil
}

func parsePost(raw rawPost, tid int64, parent *ParsedPost, prefix string) ([]ParsedPost, error) {
	if int64(raw.TID) != tid || raw.Floor == nil || *raw.Floor < 0 || raw.Author.UID == nil {
		return nil, logging.WithStack(errors.New("NGA post is missing its TID, floor, or author, or its TID differs from the request"))
	}
	p := ParsedPost{RawPayload: raw.Raw, TID: tid, PID: int64(raw.PID), Floor: int64(*raw.Floor), AuthorUID: int64(*raw.Author.UID),
		Author: raw.Author.Name, Subject: raw.Subject, Body: raw.Content, Kind: "reply", Key: "pid:" + fmt.Sprint(raw.PID), Resources: []string{}}
	if parent != nil {
		p.Kind, p.ParentKey, p.ParentFloor = "comment", parent.Key, parent.Floor
		if parent.Kind == "comment" {
			p.ParentFloor = parent.ParentFloor
		}
		p.CommentToID = strings.Trim(string(raw.CommentToID), "\"")
	} else if p.Floor == 0 {
		p.Kind, p.Key = "main", "main"
	}
	if p.Kind != "main" && p.PID <= 0 {
		return nil, logging.WithStack(errors.New("NGA reply or comment is missing a valid PID"))
	}
	if raw.Timestamp != nil {
		published := time.Unix(int64(*raw.Timestamp), 0).UTC()
		p.PublishedAt = &published
	} else if raw.Postdate != "" {
		// NGA 文本日期使用北京时间，与部署时区无关。
		for _, layout := range []string{"2006-01-02 15:04:05", "2006-01-02 15:04"} {
			if parsed, err := time.ParseInLocation(layout, raw.Postdate, time.FixedZone("NGA", 8*60*60)); err == nil {
				parsed = parsed.UTC()
				p.PublishedAt = &parsed
				break
			}
		}
		if p.PublishedAt == nil {
			return nil, logging.WithStack(errors.New("invalid NGA post timestamp"))
		}
	}
	p.SourceURL = fmt.Sprintf("%s/read.php?tid=%d", ngaBaseURL, tid)
	if p.Kind != "main" {
		p.SourceURL += fmt.Sprintf("&pid=%d", p.PID)
	}
	for _, attachment := range raw.Attachments {
		path := attachment.URL
		if path == "" {
			path = attachment.Path
		}
		p.Resources = appendResource(p.Resources, path, prefix)
		name := attachment.OriginalName
		if name == "" {
			name = attachment.Name
		}
		if resolved := appendResource(nil, path, prefix); name != "" && len(resolved) > 0 {
			if p.ResourceNames == nil {
				p.ResourceNames = map[string]string{}
			}
			p.ResourceNames[resolved[0]] = name
		}
		p.Resources = appendResource(p.Resources, attachment.Thumb, prefix)
	}
	for _, match := range resourceTag.FindAllStringSubmatch(p.Body, -1) {
		p.Resources = appendResource(p.Resources, match[1], prefix)
	}
	posts := []ParsedPost{p}
	for _, comment := range raw.Comments {
		children, err := parsePost(comment, tid, &p, prefix)
		if err != nil {
			return nil, err
		}
		posts = append(posts, children...)
	}
	return posts, nil
}

var resourceTag = regexp.MustCompile(`(?is)\[(?:img|audio|video)(?:=[^\]]*)?\]([^\[]+)\[/(?:img|audio|video)\]`)

func appendResource(resources []string, path, prefix string) []string {
	path = strings.TrimSpace(html.UnescapeString(path))
	if path == "" {
		return resources
	}
	base, err := url.Parse(prefix)
	if err != nil {
		return resources
	}
	if strings.HasPrefix(path, "./") {
		path = strings.TrimPrefix(path, "./")
	}
	if strings.HasPrefix(path, "//") {
		path = "https:" + path
	}
	u, err := url.Parse(path)
	if err != nil {
		return resources
	}
	if !u.IsAbs() {
		u = base.ResolveReference(u)
	}
	if u.Scheme != "https" && u.Scheme != "http" {
		return resources
	}
	for _, existing := range resources {
		if existing == u.String() {
			return resources
		}
	}
	return append(resources, u.String())
}
