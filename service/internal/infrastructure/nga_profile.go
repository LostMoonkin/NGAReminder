package infrastructure

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"strings"

	"github.com/rs/zerolog"
	"golang.org/x/text/encoding/simplifiedchinese"

	"ngareminder/service/internal/logging"
)

var ErrNGAUserMissing = errors.New("NGA user does not exist")

type UserProfile struct {
	UID      int64
	Username string
}

func (n *NGA) UserProfile(ctx context.Context, credentials Credentials, uid int64) (profile UserProfile, err error) {
	ctx, span := logging.Start(ctx, "infrastructure.nga.user_profile")
	defer span.End(&err)
	zerolog.Ctx(ctx).Info().Int64("uid", uid).Msg("Fetching NGA user profile")
	body, err := n.requestBytes(ctx, http.MethodGet, fmt.Sprintf("/nuke.php?func=ucp&uid=%d", uid), "", credentials.Cookie)
	if err != nil {
		return profile, err
	}
	return parseUserProfile(body, uid)
}

func parseUserProfile(body []byte, uid int64) (profile UserProfile, err error) {
	decoded, err := simplifiedchinese.GBK.NewDecoder().Bytes(body)
	if err != nil {
		return profile, logging.Wrap(err, "decode NGA user profile GBK")
	}
	_, after, ok := strings.Cut(string(decoded), "__UCPUSER")
	start := strings.IndexByte(after, '{')
	if !ok || start < 0 {
		return profile, logging.Wrap(ErrNGAUserMissing, "NGA user profile is missing its user object")
	}
	var raw struct {
		UID      number `json:"uid"`
		Username string `json:"username"`
	}
	// 只解析赋值后的第一个 JSON 对象，忽略后续脚本；不执行页面 JavaScript。
	if err = json.NewDecoder(strings.NewReader(after[start:])).Decode(&raw); err != nil {
		return profile, logging.Wrap(err, "decode NGA user profile object")
	}
	if int64(raw.UID) != uid || uid <= 0 {
		return profile, logging.Wrap(ErrNGAUserMissing, "NGA user profile does not match the requested UID")
	}
	return UserProfile{UID: uid, Username: raw.Username}, nil
}
