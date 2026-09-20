package handler

import (
	"errors"
	"fmt"
	"net/http"
	"strconv"
	"strings"

	"github.com/gin-gonic/gin"

	"ngareminder/service/internal/infrastructure"
	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/repository"
	"ngareminder/service/internal/service"
)

func (h *Handler) monitoringRoutes(router *gin.Engine) {
	web := router.Group("/admin")
	web.POST("/nga-account", h.saveAccount)
	web.POST("/nga-account/check", h.checkAccount)
	web.POST("/watches", h.saveWatch)
	web.GET("/watches/:id", h.watch)
	web.POST("/watches/:id", h.saveWatch)
	web.GET("/threads/:tid", h.posts)
	api := router.Group("/api/v1", h.authorizeAPI)
	api.GET("/nga-account", h.account)
	api.PUT("/nga-account", h.saveAccount)
	api.POST("/nga-account/check", h.checkAccount)
	api.GET("/watches", h.overview)
	api.POST("/watches", h.saveWatch)
	api.GET("/watches/:id", h.watch)
	api.PUT("/watches/:id", h.saveWatch)
	api.DELETE("/watches/:id", h.watchAction("delete"))
	api.GET("/runs/:id", h.run)
	api.GET("/threads/:tid/posts", h.posts)
	for _, action := range []string{"pause", "resume", "reset", "delete", "run"} {
		web.POST("/watches/:id/"+action, h.watchAction(action))
		if action != "delete" {
			api.POST("/watches/:id/"+action, h.watchAction(action))
		}
	}
}

func (h *Handler) overview(c *gin.Context) {
	data, err := h.monitor.Overview(c.Request.Context())
	if err != nil {
		h.problem(c, err)
		return
	}
	c.JSON(http.StatusOK, data)
}

func (h *Handler) account(c *gin.Context) {
	data, err := h.monitor.Account(c.Request.Context())
	if err != nil {
		h.problem(c, err)
		return
	}
	c.JSON(http.StatusOK, data)
}

func (h *Handler) saveAccount(c *gin.Context) {
	var input service.AccountInput
	if err := c.ShouldBind(&input); err != nil {
		// binder 错误可能含凭据，保留可操作的格式错误，不打印原始表单。
		h.problem(c, service.InvalidInput("账号参数格式无效，请填写完整 Cookie 或 passport UID/CID"))
		return
	}
	data, err := h.monitor.SaveAccount(c.Request.Context(), input, false)
	if err != nil {
		h.problem(c, err)
		return
	}
	h.respond(c, http.StatusOK, data, "/admin/settings")
}

func (h *Handler) checkAccount(c *gin.Context) {
	data, err := h.monitor.SaveAccount(c.Request.Context(), service.AccountInput{}, true)
	if err != nil {
		h.problem(c, err)
		return
	}
	h.respond(c, http.StatusOK, data, "/admin/settings")
}

func (h *Handler) saveWatch(c *gin.Context) {
	var input service.WatchInput
	if err := c.ShouldBind(&input); err != nil {
		h.problem(c, service.InvalidInput("监控参数格式无效"))
		return
	}
	if !strings.HasPrefix(c.ContentType(), "application/json") {
		if err := bindNotificationForm(c, &input); err != nil {
			h.problem(c, err)
			return
		}
		if err := bindScheduleForm(c, &input); err != nil {
			h.problem(c, err)
			return
		}
	}
	id := int64(0)
	if c.Param("id") != "" {
		var ok bool
		id, ok = h.id(c, "id")
		if !ok {
			return
		}
	}
	watch, err := h.monitor.SaveWatch(c.Request.Context(), id, input)
	if err != nil {
		h.problem(c, err)
		return
	}
	status := http.StatusOK
	if id == 0 {
		status = http.StatusCreated
	}
	destination := fmt.Sprintf("/admin/watches/%d", watch.ID)
	if id != 0 {
		destination += "#configuration"
	}
	h.respond(c, status, watch, destination)
}

func (h *Handler) watch(c *gin.Context) {
	id, ok := h.id(c, "id")
	if !ok {
		return
	}
	data, err := h.monitor.Watch(c.Request.Context(), id)
	if err != nil {
		h.problem(c, err)
		return
	}
	if isAPI(c) {
		c.JSON(http.StatusOK, data)
		return
	}
	if len(data.Runs) > 0 {
		data.Watch.LastRun = &data.Runs[0]
	}
	h.render(c, http.StatusOK, "watch", gin.H{"Data": data, "TraceID": logging.TraceID(c.Request.Context())})
}

func (h *Handler) watchAction(action string) gin.HandlerFunc {
	return func(c *gin.Context) {
		id, ok := h.id(c, "id")
		if !ok {
			return
		}
		destination := fmt.Sprintf("/admin/watches/%d", id)
		if action == "run" {
			run, err := h.monitor.StartRun(c.Request.Context(), id)
			if err != nil {
				h.problem(c, err)
				return
			}
			c.Header("Location", fmt.Sprintf("/api/v1/runs/%d", run.ID))
			h.respond(c, http.StatusAccepted, run, destination+"#runs")
			return
		}
		var input struct {
			InitMode string `json:"init_mode" form:"init_mode"`
		}
		if action == "reset" {
			if err := c.ShouldBind(&input); err != nil {
				h.problem(c, service.InvalidInput("初始化模式参数格式无效"))
				return
			}
		}
		watch, err := h.monitor.ChangeWatch(c.Request.Context(), id, action, input.InitMode)
		if err != nil {
			h.problem(c, err)
			return
		}
		if action == "delete" {
			destination = "/admin/watches"
		}
		h.respond(c, http.StatusOK, watch, destination)
	}
}

func (h *Handler) run(c *gin.Context) {
	id, ok := h.id(c, "id")
	if !ok {
		return
	}
	data, err := h.monitor.Run(c.Request.Context(), id)
	if err != nil {
		h.problem(c, err)
		return
	}
	c.JSON(http.StatusOK, data)
}

func (h *Handler) posts(c *gin.Context) {
	tid, ok := h.id(c, "tid")
	if !ok {
		return
	}
	page, err := strconv.Atoi(c.DefaultQuery("page", "1"))
	if err != nil || page < 1 || page > int(^uint(0)>>1)/50 {
		h.problem(c, service.InvalidInput("page 必须为有效正整数"))
		return
	}
	data, err := h.monitor.Content(c.Request.Context(), "threads", tid, page)
	if err != nil {
		h.problem(c, err)
		return
	}
	if isAPI(c) {
		c.JSON(http.StatusOK, data)
		return
	}
	previous, next := 0, 0
	if page > 1 {
		previous = page - 1
	}
	if int64(page)*50 < data.Total {
		next = page + 1
	}
	h.render(c, http.StatusOK, "posts", gin.H{"Data": data, "Previous": previous, "Next": next, "TraceID": logging.TraceID(c.Request.Context())})
}

func (h *Handler) id(c *gin.Context, key string) (int64, bool) {
	id, err := strconv.ParseInt(c.Param(key), 10, 64)
	if err != nil || id <= 0 {
		h.problem(c, service.InvalidInput("ID / TID 必须为正整数"))
		return 0, false
	}
	return id, true
}

func isAPI(c *gin.Context) bool { return strings.HasPrefix(c.Request.URL.Path, "/api/") }

func (h *Handler) respond(c *gin.Context, status int, data any, destination string) {
	if isAPI(c) {
		c.JSON(status, data)
	} else {
		c.Redirect(http.StatusSeeOther, destination)
	}
}

func (h *Handler) problem(c *gin.Context, err error) {
	status := http.StatusInternalServerError
	var input *service.InputError
	switch {
	case errors.As(err, &input), errors.Is(err, infrastructure.ErrCredentials):
		status = http.StatusBadRequest
	case errors.Is(err, repository.ErrNotFound):
		status = http.StatusNotFound
	case errors.Is(err, service.ErrBusy), errors.Is(err, service.ErrPaused), errors.Is(err, service.ErrBackgroundDisabled):
		status = http.StatusConflict
	case errors.Is(err, infrastructure.ErrNGAAuth):
		status = http.StatusUnprocessableEntity
	case errors.Is(err, infrastructure.ErrNGABusy), errors.Is(err, infrastructure.ErrNGASearchUnavailable):
		status = http.StatusBadGateway
	}
	message := service.FailureMessage(err)
	if isAPI(c) {
		fail(c, status, message, err)
		return
	}
	_ = c.Error(err)
	c.Abort()
	h.render(c, status, "problem", gin.H{"Message": message, "TraceID": logging.TraceID(c.Request.Context())})
}

func statusText(value string) string { return service.StatusText(value) }

// 管理页以普通表单行编辑规则，API 使用 JSON 数组；校验和规则语义都留在 service。
func bindScheduleForm(c *gin.Context, input *service.WatchInput) error {
	// 旧表单没有调度字段时仍保留原规则。
	if c.PostForm("schedule_present") != "1" {
		return nil
	}
	input.IntervalRules = []repository.IntervalRule{}
	input.NoFetchPeriods = []repository.TimeWindow{}
	for _, prefix := range []string{"interval", "period"} {
		days := c.PostFormArray(prefix + "_weekdays")
		starts, ends := c.PostFormArray(prefix+"_start"), c.PostFormArray(prefix+"_end")
		seconds := c.PostFormArray("interval_override_seconds")
		if len(days) != len(starts) || len(days) != len(ends) || prefix == "interval" && len(days) != len(seconds) {
			return service.InvalidInput("时段规则不完整")
		}
		for i, text := range days {
			window := repository.TimeWindow{Start: starts[i], End: ends[i]}
			for _, part := range strings.Split(text, ",") {
				day, err := strconv.Atoi(strings.TrimSpace(part))
				if err != nil {
					return service.InvalidInput("星期请使用逗号分隔的 1～7，例如 1,2,3,4,5")
				}
				window.Weekdays = append(window.Weekdays, day)
			}
			if prefix == "period" {
				input.NoFetchPeriods = append(input.NoFetchPeriods, window)
			} else {
				interval, err := strconv.Atoi(seconds[i])
				if err != nil {
					return service.InvalidInput("覆盖间隔必须为整数秒")
				}
				input.IntervalRules = append(input.IntervalRules, repository.IntervalRule{TimeWindow: window, IntervalSeconds: interval})
			}
		}
	}
	return nil
}

func weekdayList(days []int) string {
	values := make([]string, len(days))
	for i, day := range days {
		values[i] = strconv.Itoa(day)
	}
	return strings.Join(values, ",")
}
