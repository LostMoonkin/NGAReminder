package infrastructure

import (
	"context"
	"encoding/json"
	"fmt"
	"github.com/larksuite/oapi-sdk-go/v3/event/dispatcher"
	larkim "github.com/larksuite/oapi-sdk-go/v3/service/im/v1"
	larkws "github.com/larksuite/oapi-sdk-go/v3/ws"
	"ngareminder/service/internal/logging"
)

type FeishuReceiver func(context.Context, *larkim.P2MessageReceiveV1) error

func (n *Notifier) ConnectBot(ctx context.Context, app AppCredentials, receive FeishuReceiver, ready func(), failed func(error)) (err error) {
	ctx, span := logging.Start(ctx, "infrastructure.feishu.connect_bot")
	defer span.End(&err)
	events := dispatcher.NewEventDispatcher("", "").OnP2MessageReceiveV1(receive)
	client := larkws.NewClient(app.AppID, app.AppSecret, larkws.WithEventHandler(events), larkws.WithHttpClient(n), larkws.WithLogger(sdkQuiet{}), larkws.WithOnReady(ready), larkws.WithOnError(func(err error) { failed(logging.Wrap(err, "Feishu connection interrupted")) }))
	return logging.Wrap(client.Start(ctx), "run Feishu bot connection")
}
func (n *Notifier) Reply(ctx context.Context, app AppCredentials, messageID, text, uuid string) (err error) {
	ctx, span := logging.Start(ctx, "infrastructure.feishu.reply")
	defer span.End(&err)
	raw, _ := json.Marshal(map[string]string{"text": text})
	result, err := n.Client(app).Im.V1.Message.Reply(ctx, larkim.NewReplyMessageReqBuilder().MessageId(messageID).Body(larkim.NewReplyMessageReqBodyBuilder().MsgType("text").Content(string(raw)).Uuid(uuid).Build()).Build())
	if err != nil {
		return logging.Wrap(err, "reply to Feishu message")
	}
	if !result.Success() {
		return logging.WithStack(fmt.Errorf("Feishu reply API returned code %d", result.Code))
	}
	return nil
}
