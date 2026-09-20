package handler

import (
	"fmt"
	"strconv"
	"strings"

	"github.com/gin-gonic/gin"
	"ngareminder/service/internal/infrastructure"
	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/service"
)

func (h *Handler) notificationRoutes(router *gin.Engine) {
	router.GET("/admin/inbox", h.notifications)
	router.GET("/admin/channels", h.notifications)
	for _, prefix := range []string{"/admin", "/api/v1"} {
		group := router.Group(prefix)
		if prefix == "/api/v1" {
			group.Use(h.authorizeAPI)
		}
		group.GET("/notifications", h.notifications)
		group.POST("/feishu-app", h.saveFeishuApp)
		group.POST("/channels", h.saveChannel)
		group.POST("/channels/:id", h.saveChannel)
		for _, action := range []string{"enable", "disable", "delete", "test"} {
			group.POST("/channels/:id/"+action, h.channelAction(action))
		}
		group.POST("/inbox/:id/read", h.markRead(true))
		group.POST("/inbox/:id/unread", h.markRead(false))
		group.POST("/deliveries/:id/retry", h.retryDelivery)
	}
}
func (h *Handler) notifications(c *gin.Context) {
	page, err := strconv.Atoi(c.DefaultQuery("page", "1"))
	if err != nil || page < 1 || page > int(^uint(0)>>1)/50 {
		h.problem(c, service.InvalidInput("页码必须为有效正整数"))
		return
	}
	filter := "all"
	if !isAPI(c) {
		filter = c.DefaultQuery("filter", "all")
	}
	data, err := h.monitor.Notifications().FilteredOverview(c.Request.Context(), page, filter)
	if err != nil {
		h.problem(c, err)
		return
	}
	if isAPI(c) {
		c.JSON(200, data)
		return
	}
	name := "notifications"
	if c.FullPath() == "/admin/channels" {
		name = "channels"
	}
	view := gin.H{"Data": data, "Page": page, "Next": page + 1, "Previous": page - 1, "Filter": filter, "TraceID": logging.TraceID(c.Request.Context())}
	selected, _ := strconv.ParseInt(c.Query("event"), 10, 64)
	if len(data.Events) > 0 {
		view["Event"] = data.Events[0]
		for _, event := range data.Events {
			if event.ID == selected {
				view["Event"] = event
				break
			}
		}
	}
	h.render(c, 200, name, view)
}
func (h *Handler) saveFeishuApp(c *gin.Context) {
	var input infrastructure.AppCredentials
	if isAPI(c) {
		if c.ShouldBindJSON(&input) != nil {
			h.problem(c, service.InvalidInput("飞书应用配置格式无效"))
			return
		}
	} else {
		input.AppID = c.PostForm("app_id")
		input.AppSecret = c.PostForm("app_secret")
	}
	data, err := h.monitor.Notifications().SaveApp(c.Request.Context(), input)
	if err != nil {
		h.problem(c, err)
		return
	}
	h.respond(c, 200, data, "/admin/channels")
}
func (h *Handler) saveChannel(c *gin.Context) {
	var input service.ChannelInput
	if isAPI(c) {
		if c.ShouldBindJSON(&input) != nil {
			h.problem(c, service.InvalidInput("通知渠道参数格式无效"))
			return
		}
	} else {
		input.Name, input.Kind, input.Enabled = c.PostForm("name"), c.PostForm("kind"), c.PostForm("enabled") == "true"
		input.ChannelTarget = infrastructure.ChannelTarget{ServerURL: c.PostForm("server_url"), DeviceKey: c.PostForm("device_key"), Group: c.PostForm("group"), ReceiveID: c.PostForm("receive_id"), ReceiveIDType: c.PostForm("receive_id_type")}
	}
	id := int64(0)
	if c.Param("id") != "" {
		var ok bool
		id, ok = h.id(c, "id")
		if !ok {
			return
		}
	}
	data, err := h.monitor.Notifications().SaveChannel(c.Request.Context(), id, input)
	if err != nil {
		h.problem(c, err)
		return
	}
	status := 200
	if id == 0 {
		status = 201
	}
	h.respond(c, status, data, "/admin/channels")
}
func (h *Handler) channelAction(action string) gin.HandlerFunc {
	return func(c *gin.Context) {
		id, ok := h.id(c, "id")
		if !ok {
			return
		}
		var err error
		if action == "test" {
			err = h.monitor.Notifications().TestChannel(c.Request.Context(), id)
		} else {
			err = h.monitor.Notifications().ChangeChannel(c.Request.Context(), id, action)
		}
		if err != nil {
			h.problem(c, err)
			return
		}
		h.respond(c, 200, gin.H{"status": "ok"}, "/admin/channels")
	}
}
func (h *Handler) markRead(read bool) gin.HandlerFunc {
	return func(c *gin.Context) {
		id, ok := h.id(c, "id")
		if !ok {
			return
		}
		if err := h.monitor.Notifications().MarkRead(c.Request.Context(), id, read); err != nil {
			h.problem(c, err)
			return
		}
		h.respond(c, 200, gin.H{"read": read}, inboxReturn(c))
	}
}
func (h *Handler) retryDelivery(c *gin.Context) {
	id, ok := h.id(c, "id")
	if !ok {
		return
	}
	if err := h.monitor.Notifications().Retry(c.Request.Context(), id); err != nil {
		h.problem(c, err)
		return
	}
	h.respond(c, 200, gin.H{"status": "pending"}, inboxReturn(c))
}
func bindNotificationForm(c *gin.Context, input *service.WatchInput) error {
	if c.PostForm("notifications_present") != "1" {
		return nil
	}
	if input.ChannelIDs == nil {
		input.ChannelIDs = []int64{}
	}
	input.AuthorUIDs = []int64{}
	for _, part := range strings.FieldsFunc(c.PostForm("author_uids"), func(r rune) bool { return r == ',' || r == ' ' || r == '\n' }) {
		id, err := strconv.ParseInt(part, 10, 64)
		if err != nil || id <= 0 {
			return service.InvalidInput("作者白名单请填写逗号分隔的正整数 UID")
		}
		input.AuthorUIDs = append(input.AuthorUIDs, id)
	}
	return nil
}
func intList(values []int64) string {
	parts := []string{}
	for _, v := range values {
		parts = append(parts, strconv.FormatInt(v, 10))
	}
	return strings.Join(parts, ",")
}
func containsID(values []int64, id int64) bool {
	for _, v := range values {
		if v == id {
			return true
		}
	}
	return false
}

// 重定向只组合固定站内路径与受限参数，不接受任意 return URL。
func inboxReturn(c *gin.Context) string {
	page, _ := strconv.Atoi(c.PostForm("return_page"))
	if page < 1 || page > int(^uint(0)>>1)/50 {
		page = 1
	}
	filter := c.PostForm("return_filter")
	switch filter {
	case "all", "unread", "delivery", "alerts":
	default:
		filter = "all"
	}
	event, _ := strconv.ParseInt(c.PostForm("return_event"), 10, 64)
	if event < 0 {
		event = 0
	}
	return fmt.Sprintf("/admin/inbox?page=%d&filter=%s&event=%d", page, filter, event)
}
