package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	stdlog "log"
	"net"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/rs/zerolog"

	"ngareminder/service/internal/config"
	"ngareminder/service/internal/handler"
	"ngareminder/service/internal/infrastructure"
	"ngareminder/service/internal/logging"
	"ngareminder/service/internal/repository"
	"ngareminder/service/internal/service"
)

func main() { os.Exit(execute()) }

func execute() (exitCode int) {
	log := logging.New(os.Stdout)
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	ctx, span := logging.Start(log.WithContext(ctx), "process")
	var err error
	defer func() {
		if value := recover(); value != nil {
			err = logging.FromPanic(value)
			exitCode = 1
		}
		if err != nil {
			logging.Error(ctx, err, "服务启动或运行失败", zerolog.ErrorLevel)
		}
		span.End(&err)
	}()
	flags := flag.NewFlagSet("nga-reminder", flag.ContinueOnError)
	flags.SetOutput(io.Discard)
	filename := flags.String("config", "", "JSON 配置文件；省略时使用默认值和环境变量")
	if err = flags.Parse(os.Args[1:]); errors.Is(err, flag.ErrHelp) {
		fmt.Fprintln(os.Stdout, "用法：nga-reminder [-config config.json]\n配置字段可通过 NGA_REMINDER_ 前缀的环境变量覆盖，见 service/docs/README.md。")
		err = nil
		return 0
	} else if err != nil {
		err = logging.Wrap(err, "解析命令行参数")
		return 1
	}
	if flags.NArg() != 0 {
		err = logging.WithStack(errors.New("不接受位置参数，请使用 -config 指定配置文件"))
		return 1
	}
	cfg, loadErr := config.Load(ctx, *filename)
	log.SetSecrets(cfg.Secrets()...)
	if loadErr != nil {
		err = loadErr
		return 1
	}
	if err = run(ctx, log, cfg); err != nil {
		return 1
	}
	return 0
}

func run(ctx context.Context, log *logging.Logger, cfg config.Config) (err error) {
	if err = infrastructure.PrepareStorage(ctx, cfg.DatabasePath, cfg.AssetsPath); err != nil {
		return err
	}
	store, err := repository.Open(ctx, cfg.DatabasePath)
	if err != nil {
		return err
	}
	defer func() { err = errors.Join(err, store.Close(ctx)) }()
	admin, err := service.NewAdmin(ctx, cfg, store)
	if err != nil {
		return err
	}
	router, err := handler.New(admin, log, cfg.CookieSecure)
	if err != nil {
		return err
	}
	listener, err := net.Listen("tcp", cfg.ListenAddress)
	if err != nil {
		return logging.Wrap(err, "监听 HTTP 地址")
	}
	server := &http.Server{
		Handler: router, ReadHeaderTimeout: 5 * time.Second, ReadTimeout: 15 * time.Second,
		WriteTimeout: 30 * time.Second, IdleTimeout: 60 * time.Second,
		ErrorLog: stdlog.New(logging.StandardErrors(ctx), "", 0),
	}
	result := make(chan error, 1)
	go func() {
		serveErr := server.Serve(listener)
		if errors.Is(serveErr, http.ErrServerClosed) {
			serveErr = nil
		}
		result <- logging.Wrap(serveErr, "HTTP 服务退出")
	}()
	// 本阶段尚无后台业务；后续调度和 Bot 只能在此开关开启时启动。
	zerolog.Ctx(ctx).Info().Str("event", "server_started").Str("listen_address", listener.Addr().String()).
		Bool("background_enabled", cfg.BackgroundEnabled).Msg("HTTP 服务已启动")
	select {
	case err = <-result:
		return err
	case <-ctx.Done():
		zerolog.Ctx(ctx).Info().Msg("停止接收请求，等待当前请求结束")
		shutdownCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		if err = server.Shutdown(shutdownCtx); err != nil {
			return errors.Join(logging.Wrap(err, "HTTP 停止超时"), logging.Wrap(server.Close(), "强制关闭 HTTP"))
		}
		return <-result
	}
}
