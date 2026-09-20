package handler

import (
	"fmt"
	"net/http"

	"github.com/gin-gonic/gin"
	"ngareminder/service/internal/repository"
)

// 只负责页面导航与看板展示，采集和调度规则继续由 service 决定。
type pageMeta struct{ title, nav, section string }

func adminPageMeta(name string) pageMeta {
	switch name {
	case "admin":
		return pageMeta{"运行概览", "overview", ""}
	case "watches":
		return pageMeta{"监控管理", "watches", ""}
	case "new-watch":
		return pageMeta{"新建监控", "watches", ""}
	case "watch":
		return pageMeta{"监控详情", "watches", ""}
	case "library", "users", "posts":
		return pageMeta{"内容库", "library", ""}
	case "notifications":
		return pageMeta{"收件箱", "inbox", ""}
	case "channels":
		return pageMeta{"通知渠道", "channels", ""}
	case "account":
		return pageMeta{"NGA 账号", "settings", "account"}
	case "renewal":
		return pageMeta{"Cookie 续期", "settings", "renewal"}
	case "bot":
		return pageMeta{"飞书 Bot", "settings", "bot"}
	case "resources":
		return pageMeta{"资源维护", "settings", "resources"}
	case "runtime":
		return pageMeta{"运行配置", "settings", "runtime"}
	default:
		return pageMeta{"操作未完成", "", ""}
	}
}

type watchColumn struct {
	Key, Title string
	Watches    []repository.Watch
}

func watchGroup(w repository.Watch) string {
	if w.Paused || w.State != "ready" {
		return "paused"
	}
	if w.LastRun != nil && w.LastRun.Status == "running" {
		return "active"
	}
	if w.NoFetch {
		return "quiet"
	}
	return "active"
}

func watchTitle(w repository.Watch) string {
	if w.Title != "" {
		return w.Title
	}
	if w.Label != "" {
		return w.Label
	}
	if w.Kind == "uid" {
		return fmt.Sprintf("UID %d", w.UID)
	}
	return fmt.Sprintf("TID %d", w.TID)
}

func (h *Handler) accountSettings(c *gin.Context) {
	account, err := h.monitor.Account(c.Request.Context())
	if err != nil {
		h.problem(c, err)
		return
	}
	settings, err := h.admin.Settings(c.Request.Context())
	if err != nil {
		h.problem(c, err)
		return
	}
	h.render(c, http.StatusOK, "account", gin.H{"Account": account, "Settings": settings})
}

func (h *Handler) runtimeSettings(c *gin.Context) {
	settings, err := h.admin.Settings(c.Request.Context())
	if err != nil {
		h.problem(c, err)
		return
	}
	h.render(c, http.StatusOK, "runtime", gin.H{"Settings": settings})
}

func (h *Handler) newWatch(c *gin.Context) {
	// 新建表单复用已有安全的渠道摘要，不读取或输出收件地址。
	data, err := h.monitor.Notifications().Overview(c.Request.Context(), 1)
	if err != nil {
		h.problem(c, err)
		return
	}
	settings, err := h.admin.Settings(c.Request.Context())
	if err != nil {
		h.problem(c, err)
		return
	}
	kind := "tid"
	if c.Query("kind") == "uid" {
		kind = "uid"
	}
	h.render(c, http.StatusOK, "new-watch", gin.H{"Kind": kind, "Channels": data.Channels, "Settings": settings})
}
