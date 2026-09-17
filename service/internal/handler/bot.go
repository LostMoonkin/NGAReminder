package handler

import (
	"context"
	"encoding/json"
	"errors"
	"strings"

	"github.com/gin-gonic/gin"
	larkim "github.com/larksuite/oapi-sdk-go/v3/service/im/v1"
	"github.com/rs/zerolog"
	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/service"
)

func (h *Handler) botRoutes(router *gin.Engine) {
	h.monitor.Bot().SetReceiver(h.receiveBot)
	for _, prefix := range []string{"/admin", "/api/v1"} {
		group := router.Group(prefix)
		if prefix == "/api/v1" {
			group.Use(h.authorizeAPI)
		}
		group.GET("/bot", h.bot)
		group.POST("/bot", h.saveBot)
		group.POST("/bot/bind-code", h.bindingCode)
		group.POST("/bot/bindings/:id/revoke", h.revokeBinding)
	}
}
func (h *Handler) bot(c *gin.Context) {
	data, err := h.monitor.Bot().Overview(c.Request.Context())
	if err != nil {
		h.problem(c, err)
		return
	}
	if isAPI(c) {
		c.JSON(200, data)
		return
	}
	h.render(c, 200, "bot", gin.H{"Data": data, "TraceID": logging.TraceID(c.Request.Context())})
}
func (h *Handler) saveBot(c *gin.Context) {
	var input struct {
		Enabled bool    `json:"enabled"`
		Groups  *string `json:"groups"`
	}
	if isAPI(c) {
		if c.ShouldBindJSON(&input) != nil {
			h.problem(c, service.InvalidInput("Bot 配置格式无效"))
			return
		}
	} else {
		input.Enabled = c.PostForm("enabled") == "true"
		if c.PostForm("replace_groups") == "true" {
			text := c.PostForm("groups")
			input.Groups = &text
		}
	}
	settings, err := h.monitor.Bot().SaveSettings(c.Request.Context(), input.Enabled, input.Groups)
	if err != nil {
		h.problem(c, err)
		return
	}
	h.respond(c, 200, settings, "/admin/bot")
}
func (h *Handler) bindingCode(c *gin.Context) {
	code, expires, err := h.monitor.Bot().GenerateCode(c.Request.Context())
	if err != nil {
		h.problem(c, err)
		return
	}
	if isAPI(c) {
		c.JSON(200, gin.H{"code": code, "expires_at": expires})
		return
	}
	data, err := h.monitor.Bot().Overview(c.Request.Context())
	if err != nil {
		h.problem(c, err)
		return
	}
	h.render(c, 200, "bot", gin.H{"Data": data, "Code": code, "Expires": expires, "TraceID": logging.TraceID(c.Request.Context())})
}
func (h *Handler) revokeBinding(c *gin.Context) {
	id, ok := h.id(c, "id")
	if !ok {
		return
	}
	if err := h.monitor.Bot().Revoke(c.Request.Context(), id); err != nil {
		h.problem(c, err)
		return
	}
	h.respond(c, 200, gin.H{"status": "ok"}, "/admin/bot")
}
func stringValue(v *string) string {
	if v == nil {
		return ""
	}
	return *v
}
func (h *Handler) receiveBot(_ context.Context, event *larkim.P2MessageReceiveV1) (err error) {
	// 每条消息建立独立 trace；不把 SDK 原始事件、绑定码或私聊正文写入日志。
	ctx, cancel := h.monitor.TaskContext()
	defer cancel()
	ctx, span := logging.Start(ctx, "handler.feishu_message")
	defer span.End(&err)
	defer func() {
		if value := recover(); value != nil {
			err = logging.FromPanic(value)
		}
		if err != nil {
			logging.Error(ctx, err, "Bot command failed", zerolog.ErrorLevel)
		}
	}()
	if event == nil || event.Event == nil || event.Event.Sender == nil || event.Event.Message == nil || event.Event.Sender.SenderId == nil || event.EventV2Base == nil || event.EventV2Base.Header == nil {
		return nil
	}
	sender, message := event.Event.Sender, event.Event.Message
	if stringValue(sender.SenderType) != "user" || stringValue(message.MessageType) != "text" {
		return nil
	}
	var body struct {
		Text string `json:"text"`
	}
	if json.Unmarshal([]byte(stringValue(message.Content)), &body) != nil {
		return service.InvalidInput("飞书文本消息格式无效")
	}
	for _, mention := range message.Mentions {
		if mention != nil && mention.Key != nil {
			body.Text = strings.ReplaceAll(body.Text, *mention.Key, "")
		}
	}
	words := strings.Fields(body.Text)
	if len(words) == 0 || !strings.HasPrefix(words[0], "/") {
		return nil
	}
	name := strings.ToLower(words[0])
	switch name {
	case "/help", "/status", "/watch", "/bind":
	default:
		name = "unknown"
	}
	cmd := service.BotCommand{AppID: event.EventV2Base.Header.AppID, MessageID: stringValue(message.MessageId), ActorID: stringValue(sender.SenderId.OpenId), ChatID: stringValue(message.ChatId), ChatType: stringValue(message.ChatType), Name: name, Args: words[1:]}
	reply, commandErr := h.monitor.Bot().Execute(ctx, cmd)
	if reply != "" {
		if e := h.monitor.Bot().Reply(ctx, cmd, reply); e != nil {
			err = e
		}
	}
	if commandErr != nil {
		err = errors.Join(err, commandErr)
	}
	return err
}
