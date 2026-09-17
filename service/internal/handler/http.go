package handler

import (
	"bytes"
	_ "embed"
	"errors"
	"html/template"
	"net/http"
	"strings"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/rs/zerolog"

	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/service"
)

//go:embed pages.html
var pages string

const sessionCookie = "nga_session"

type Handler struct {
	admin        *service.Admin
	pages        *template.Template
	cookieSecure bool
}

func New(admin *service.Admin, log *logging.Logger, cookieSecure bool) (*gin.Engine, error) {
	templates, err := template.New("pages").Parse(pages)
	if err != nil {
		return nil, logging.Wrap(err, "加载管理页")
	}
	h := &Handler{admin, templates, cookieSecure}
	gin.SetMode(gin.ReleaseMode)
	router := gin.New()
	// Gin 的自动尾斜杠重定向会跳过中间件；让所有路由结果都经过调用链日志。
	router.RedirectTrailingSlash = false
	if err = router.SetTrustedProxies(nil); err != nil {
		return nil, logging.Wrap(err, "设置 HTTP 代理策略")
	}
	router.HandleMethodNotAllowed = true
	router.Use(traceRequests(log))
	origin := http.NewCrossOriginProtection()
	router.Use(func(c *gin.Context) {
		c.Header("Cache-Control", "no-store")
		if err := origin.Check(c.Request); err != nil {
			fail(c, http.StatusForbidden, "请从本站页面提交操作", logging.Wrap(err, "跨站操作被拒绝"))
			return
		}
		c.Next()
	})
	router.GET("/", func(c *gin.Context) { c.Redirect(http.StatusFound, "/admin") })
	router.GET("/healthz", func(c *gin.Context) { c.JSON(http.StatusOK, gin.H{"status": "ok"}) })
	router.GET("/readyz", h.ready)
	router.GET("/admin/login", func(c *gin.Context) { h.render(c, http.StatusOK, "login", nil) })
	router.POST("/admin/login", h.login)
	router.POST("/admin/logout", h.logout)
	router.GET("/admin", h.authorize(false), h.dashboard)
	router.GET("/api/v1/settings", h.authorize(true), h.settings)
	router.NoRoute(func(c *gin.Context) {
		fail(c, http.StatusNotFound, "页面或接口不存在", logging.WithStack(errors.New("路由不存在")))
	})
	router.NoMethod(func(c *gin.Context) {
		fail(c, http.StatusMethodNotAllowed, "请求方法不支持", logging.WithStack(errors.New("请求方法不支持")))
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
				logging.Error(ctx, err, "HTTP 请求未完成预期操作", level)
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

func (h *Handler) authorize(api bool) gin.HandlerFunc {
	return func(c *gin.Context) {
		var err error
		if authorization := c.GetHeader("Authorization"); api && authorization != "" {
			scheme, token, _ := strings.Cut(authorization, " ")
			if !strings.EqualFold(scheme, "Bearer") {
				token = ""
			}
			err = h.admin.CheckToken(c.Request.Context(), token)
		} else {
			token, _ := c.Cookie(sessionCookie)
			err = h.admin.CheckSession(c.Request.Context(), token)
		}
		if errors.Is(err, service.ErrUnauthorized) {
			if api {
				fail(c, http.StatusUnauthorized, "请登录或提供有效的 API token", err)
			} else {
				_ = c.Error(err)
				c.Redirect(http.StatusSeeOther, "/admin/login")
				c.Abort()
			}
			return
		}
		if err != nil {
			fail(c, http.StatusServiceUnavailable, "暂时无法验证会话，请凭关联 ID 查看日志", err)
			return
		}
		c.Next()
	}
}

func (h *Handler) login(c *gin.Context) {
	c.Request.Body = http.MaxBytesReader(c.Writer, c.Request.Body, 16<<10)
	if err := c.Request.ParseForm(); err != nil {
		fail(c, http.StatusBadRequest, "登录表单无效或过大", logging.Wrap(err, "读取登录表单"))
		return
	}
	token, expires, err := h.admin.Login(c.Request.Context(), c.PostForm("username"), c.PostForm("password"))
	if errors.Is(err, service.ErrUnauthorized) {
		_ = c.Error(err)
		h.render(c, http.StatusUnauthorized, "login", gin.H{"Error": "用户名或密码不正确", "TraceID": logging.TraceID(c.Request.Context())})
		return
	}
	if err != nil {
		fail(c, http.StatusServiceUnavailable, "登录暂时不可用，请凭关联 ID 查看日志", err)
		return
	}
	h.setCookie(c, token, expires, int(service.SessionLifetime.Seconds()))
	c.Redirect(http.StatusSeeOther, "/admin")
}

func (h *Handler) logout(c *gin.Context) {
	token, _ := c.Cookie(sessionCookie)
	if err := h.admin.Logout(c.Request.Context(), token); err != nil {
		fail(c, http.StatusServiceUnavailable, "退出未完成，请稍后重试", err)
		return
	}
	h.setCookie(c, "", time.Unix(1, 0), -1)
	c.Redirect(http.StatusSeeOther, "/admin/login")
}

func (h *Handler) setCookie(c *gin.Context, token string, expires time.Time, maxAge int) {
	http.SetCookie(c.Writer, &http.Cookie{Name: sessionCookie, Value: token, Path: "/", HttpOnly: true,
		Secure: h.cookieSecure || c.Request.TLS != nil, SameSite: http.SameSiteStrictMode, MaxAge: maxAge, Expires: expires})
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
	h.render(c, http.StatusOK, "admin", gin.H{"Settings": settings, "TraceID": logging.TraceID(c.Request.Context())})
}

func (h *Handler) render(c *gin.Context, status int, name string, data any) {
	var body bytes.Buffer
	if err := h.pages.ExecuteTemplate(&body, name, data); err != nil {
		fail(c, http.StatusInternalServerError, "页面暂时不可用", logging.Wrap(err, "渲染管理页"))
		return
	}
	c.Data(status, "text/html; charset=utf-8", body.Bytes())
}
