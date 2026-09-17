package handler

import (
	"github.com/gin-gonic/gin"
	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/service"
)

func (h *Handler) renewalRoutes(router *gin.Engine) {
	for _, prefix := range []string{"/admin", "/api/v1"} {
		group := router.Group(prefix)
		if prefix == "/api/v1" {
			group.Use(h.authorizeAPI)
		}
		group.GET("/renewal", h.renewal)
		group.POST("/renewal", h.saveRenewal)
		group.POST("/renewal/start", h.startRenewal)
	}
}
func (h *Handler) renewal(c *gin.Context) {
	data, err := h.monitor.Renewal().Overview(c.Request.Context())
	if err != nil {
		h.problem(c, err)
		return
	}
	if isAPI(c) {
		c.JSON(200, data)
		return
	}
	h.render(c, 200, "renewal", gin.H{"Data": data, "TraceID": logging.TraceID(c.Request.Context())})
}
func (h *Handler) saveRenewal(c *gin.Context) {
	var input service.RenewalInput
	var bindErr error
	if isAPI(c) {
		bindErr = c.ShouldBindJSON(&input)
	} else {
		bindErr = c.ShouldBind(&input)
	}
	if bindErr != nil {
		h.problem(c, service.InvalidInput("续期配置格式无效"))
		return
	}
	result, err := h.monitor.Renewal().SaveSettings(c.Request.Context(), input)
	if err != nil {
		h.problem(c, err)
		return
	}
	h.respond(c, 200, result, "/admin/renewal")
}
func (h *Handler) startRenewal(c *gin.Context) {
	result, err := h.monitor.Renewal().Start(c.Request.Context())
	if err != nil {
		h.problem(c, err)
		return
	}
	h.respond(c, 200, result, "/admin/renewal")
}
