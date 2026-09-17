package handler

import (
	"fmt"
	"github.com/gin-gonic/gin"
	"ngareminder/service/internal/service"
)

func (h *Handler) recoveryRoutes(router *gin.Engine) {
	for _, prefix := range []string{"/admin", "/api/v1"} {
		group := router.Group(prefix)
		if prefix == "/api/v1" {
			group.Use(h.authorizeAPI)
		}
		group.POST("/watches/:id/backfills", h.startBackfill)
		group.GET("/backfills/:id", h.backfill)
	}
}
func (h *Handler) startBackfill(c *gin.Context) {
	id, ok := h.id(c, "id")
	if !ok {
		return
	}
	var input struct {
		StartDate string `json:"start_date" form:"start_date"`
	}
	var err error
	if isAPI(c) {
		err = c.ShouldBindJSON(&input)
	} else {
		err = c.ShouldBind(&input)
	}
	if err != nil {
		h.problem(c, service.InvalidInput("请填写回填起始日期"))
		return
	}
	result, err := h.monitor.StartBackfill(c.Request.Context(), id, input.StartDate)
	if err != nil {
		h.problem(c, err)
		return
	}
	if isAPI(c) {
		c.Header("Location", fmt.Sprintf("/api/v1/backfills/%d", result.ID))
		c.JSON(202, result)
		return
	}
	c.Redirect(303, fmt.Sprintf("/admin/watches/%d", id))
}
func (h *Handler) backfill(c *gin.Context) {
	id, ok := h.id(c, "id")
	if !ok {
		return
	}
	result, err := h.monitor.Backfill(c.Request.Context(), id)
	if err != nil {
		h.problem(c, err)
		return
	}
	c.JSON(200, result)
}
