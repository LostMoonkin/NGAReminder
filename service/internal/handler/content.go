package handler

import (
	"errors"
	"fmt"
	"github.com/gin-gonic/gin"
	"net/http"
	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/service"
	"strconv"
	"time"
)

func (h *Handler) contentRoutes(router *gin.Engine) {
	router.GET("/admin/users/:uid", h.userContent)
	router.GET("/api/v1/users/:uid/posts", h.authorizeAPI, h.userContent)
	for _, prefix := range []string{"/admin", "/api/v1"} {
		group := router.Group(prefix)
		if prefix == "/api/v1" {
			group.Use(h.authorizeAPI)
		}
		group.GET("/exports/:kind/:id", h.exportContent)
		group.GET("/resources", h.resources)
		group.POST("/resources", h.saveResourceSettings)
		group.POST("/resources/redownload", h.redownloadResources)
		group.POST("/resources/cleanup", h.cleanupResources)
		group.GET("/assets/:name", h.asset)
	}
}
func (h *Handler) userContent(c *gin.Context) {
	uid, ok := h.id(c, "uid")
	if !ok {
		return
	}
	page, err := strconv.Atoi(c.DefaultQuery("page", "1"))
	if err != nil || page < 1 || page > int(^uint(0)>>1)/50 {
		h.problem(c, service.InvalidInput("页码必须为有效正整数"))
		return
	}
	data, err := h.monitor.Content(c.Request.Context(), "users", uid, page)
	if err != nil {
		h.problem(c, err)
		return
	}
	if isAPI(c) {
		c.JSON(200, data)
		return
	}
	previous, next := 0, 0
	if page > 1 {
		previous = page - 1
	}
	if int64(page)*50 < data.Total {
		next = page + 1
	}
	h.render(c, 200, "posts", gin.H{"Data": data, "Previous": previous, "Next": next, "TraceID": logging.TraceID(c.Request.Context())})
}
func (h *Handler) exportContent(c *gin.Context) {
	if !h.longResponse(c) {
		return
	}
	id, ok := h.id(c, "id")
	if !ok {
		return
	}
	format := c.DefaultQuery("format", "markdown")
	file, err := h.monitor.Export(c.Request.Context(), c.Param("kind"), id, format)
	if err != nil {
		h.problem(c, err)
		return
	}
	defer func() {
		if e := h.monitor.RemoveExport(c.Request.Context(), file); e != nil {
			_ = c.Error(e)
		}
	}()
	extension, mime := "md", "text/markdown; charset=utf-8"
	if format == "zip" {
		extension, mime = "zip", "application/zip"
	}
	c.Header("Content-Type", mime)
	c.Header("Content-Disposition", fmt.Sprintf(`attachment; filename="%s-%d.%s"`, c.Param("kind"), id, extension))
	if err = h.monitor.SendExport(c.Request.Context(), c.Writer, file); err != nil {
		_ = c.Error(err)
	}
}
func (h *Handler) resources(c *gin.Context) {
	data, err := h.monitor.Resources().Scan(c.Request.Context())
	if err != nil {
		h.problem(c, err)
		return
	}
	if isAPI(c) {
		c.JSON(200, data)
		return
	}
	h.render(c, 200, "resources", gin.H{"Data": data, "TraceID": logging.TraceID(c.Request.Context())})
}
func (h *Handler) saveResourceSettings(c *gin.Context) {
	var input struct {
		Enabled bool `json:"download_enabled" form:"download_enabled"`
	}
	var err error
	if isAPI(c) {
		err = c.ShouldBindJSON(&input)
	} else {
		err = c.ShouldBind(&input)
	}
	if err != nil {
		h.problem(c, service.InvalidInput("资源设置格式无效"))
		return
	}
	if err = h.monitor.Resources().SaveSettings(c.Request.Context(), input.Enabled); err != nil {
		h.problem(c, err)
		return
	}
	h.respond(c, 200, gin.H{"download_enabled": input.Enabled}, "/admin/resources")
}
func (h *Handler) redownloadResources(c *gin.Context) {
	if !h.longResponse(c) {
		return
	}
	count, err := h.monitor.Resources().Redownload(c.Request.Context())
	if err != nil {
		h.problem(c, err)
		return
	}
	h.respond(c, 200, gin.H{"downloaded": count}, "/admin/resources")
}
func (h *Handler) cleanupResources(c *gin.Context) {
	var input struct {
		Confirm bool `json:"confirm" form:"confirm"`
	}
	var err error
	if isAPI(c) {
		err = c.ShouldBindJSON(&input)
	} else {
		err = c.ShouldBind(&input)
	}
	if err != nil {
		h.problem(c, service.InvalidInput("请确认资源清理"))
		return
	}
	count, err := h.monitor.Resources().Cleanup(c.Request.Context(), input.Confirm)
	if err != nil {
		h.problem(c, err)
		return
	}
	h.respond(c, 200, gin.H{"removed": count}, "/admin/resources")
}
func (h *Handler) asset(c *gin.Context) {
	file, err := h.monitor.Resources().Open(c.Request.Context(), c.Param("name"))
	if err != nil {
		h.problem(c, err)
		return
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil {
		h.problem(c, logging.Wrap(err, "inspect asset response"))
		return
	}
	c.Header("X-Content-Type-Options", "nosniff")
	c.Header("Content-Disposition", "attachment")
	http.ServeContent(c.Writer, c.Request, info.Name(), info.ModTime(), file)
}

// 批量导出/资源补下载不受普通响应的 30 秒写入期限截断，仍随客户端断开取消。
func (h *Handler) longResponse(c *gin.Context) bool {
	err := http.NewResponseController(c.Writer).SetWriteDeadline(time.Time{})
	if err != nil && !errors.Is(err, http.ErrNotSupported) {
		h.problem(c, logging.Wrap(err, "clear batch response write deadline"))
		return false
	}
	return true
}
