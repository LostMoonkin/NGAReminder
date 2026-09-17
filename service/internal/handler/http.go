package handler

import (
	"bytes"
	_ "embed"
	"errors"
	"html/template"
	"net/http"
	"strings"

	"github.com/gin-gonic/gin"
	"github.com/rs/zerolog"

	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/service"
)

//go:embed pages.html
var pages string

type Handler struct {
	admin   *service.Admin
	monitor *service.Monitoring
	pages   *template.Template
}

func New(admin *service.Admin, monitor *service.Monitoring, log *logging.Logger) (*gin.Engine, error) {
	templates, err := template.New("pages").Funcs(template.FuncMap{"when": admin.FormatTime, "status": statusText, "weekdays": weekdayList}).Parse(pages)
	if err != nil {
		return nil, logging.Wrap(err, "load admin templates")
	}
	h := &Handler{admin, monitor, templates}
	gin.SetMode(gin.ReleaseMode)
	router := gin.New()
	// Gin 的自动尾斜杠重定向会跳过中间件；让所有路由结果都经过调用链日志。
	router.RedirectTrailingSlash = false
	if err = router.SetTrustedProxies(nil); err != nil {
		return nil, logging.Wrap(err, "configure trusted HTTP proxies")
	}
	router.HandleMethodNotAllowed = true
	router.Use(traceRequests(log))
	origin := http.NewCrossOriginProtection()
	router.Use(func(c *gin.Context) {
		c.Header("Cache-Control", "no-store")
		if err := origin.Check(c.Request); err != nil {
			fail(c, http.StatusForbidden, "请从本站页面提交操作", logging.Wrap(err, "cross-origin request rejected"))
			return
		}
		c.Next()
	})
	router.GET("/", func(c *gin.Context) { c.Redirect(http.StatusFound, "/admin") })
	router.GET("/healthz", func(c *gin.Context) { c.JSON(http.StatusOK, gin.H{"status": "ok"}) })
	router.GET("/readyz", h.ready)
	router.GET("/admin", h.dashboard)
	router.GET("/api/v1/settings", h.authorizeAPI, h.settings)
	h.monitoringRoutes(router)
	router.NoRoute(func(c *gin.Context) {
		fail(c, http.StatusNotFound, "页面或接口不存在", logging.WithStack(errors.New("route not found")))
	})
	router.NoMethod(func(c *gin.Context) {
		fail(c, http.StatusMethodNotAllowed, "请求方法不支持", logging.WithStack(errors.New("method not allowed")))
	})
	return router, nil
}

func traceRequests(log *logging.Logger) gin.HandlerFunc {
	return func(c *gin.Context) {
		path := c.FullPath()
		if path == "" {
			path = "unmatched"
		}
		ctx, span := logging.Start(log.WithContext(c.Request.Context()), "handler."+c.Request.Method+" "+path)
		c.Request = c.Request.WithContext(ctx)
		c.Header("X-Request-ID", span.TraceID)
		var err error
		defer func() {
			if value := recover(); value != nil {
				err = logging.FromPanic(value)
				c.Abort()
				if !c.Writer.Written() {
					c.JSON(http.StatusInternalServerError, gin.H{"error": "服务处理失败，请凭关联 ID 查看日志", "trace_id": span.TraceID})
				}
			}
			if last := c.Errors.Last(); last != nil && err == nil {
				err = last.Err
			}
			if err != nil {
				level := zerolog.WarnLevel
				if c.Writer.Status() >= 500 {
					level = zerolog.ErrorLevel
				}
				logging.Error(ctx, err, "HTTP request failed", level)
			}
			zerolog.Ctx(ctx).Info().Str("event", "http").Str("method", c.Request.Method).
				Str("route", path).Int("status", c.Writer.Status()).Int("response_bytes", c.Writer.Size()).Msg("")
			span.End(&err)
		}()
		c.Next()
	}
}

func fail(c *gin.Context, status int, message string, err error) {
	_ = c.Error(err)
	c.AbortWithStatusJSON(status, gin.H{"error": message, "trace_id": logging.TraceID(c.Request.Context())})
}

func (h *Handler) authorizeAPI(c *gin.Context) {
	scheme, token, _ := strings.Cut(c.GetHeader("Authorization"), " ")
	if !strings.EqualFold(scheme, "Bearer") {
		token = ""
	}
	if err := h.admin.CheckToken(c.Request.Context(), token); err != nil {
		fail(c, http.StatusUnauthorized, "请提供有效的 API token", err)
		return
	}
	c.Next()
}

func (h *Handler) ready(c *gin.Context) {
	if err := h.admin.Ready(c.Request.Context()); err != nil {
		fail(c, http.StatusServiceUnavailable, "数据库暂时不可用", err)
		return
	}
	c.JSON(http.StatusOK, gin.H{"status": "ok", "database": "ok"})
}

func (h *Handler) settings(c *gin.Context) {
	settings, err := h.admin.Settings(c.Request.Context())
	if err != nil {
		fail(c, http.StatusServiceUnavailable, "读取运行设置失败，请凭关联 ID 查看日志", err)
		return
	}
	c.JSON(http.StatusOK, settings)
}

func (h *Handler) dashboard(c *gin.Context) {
	settings, err := h.admin.Settings(c.Request.Context())
	if err != nil {
		fail(c, http.StatusServiceUnavailable, "读取运行设置失败，请凭关联 ID 查看日志", err)
		return
	}
	data, err := h.monitor.Overview(c.Request.Context())
	if err != nil {
		h.problem(c, err)
		return
	}
	h.render(c, http.StatusOK, "admin", gin.H{"Settings": settings, "Data": data, "TraceID": logging.TraceID(c.Request.Context())})
}

func (h *Handler) render(c *gin.Context, status int, name string, data any) {
	var body bytes.Buffer
	if err := h.pages.ExecuteTemplate(&body, name, data); err != nil {
		fail(c, http.StatusInternalServerError, "页面暂时不可用", logging.Wrap(err, "render admin page"))
		return
	}
	c.Data(status, "text/html; charset=utf-8", body.Bytes())
}
