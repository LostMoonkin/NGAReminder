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
	log := logging.NewConsole(os.Stdout)
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
			logging.Error(ctx, err, "Server startup or execution failed", zerolog.ErrorLevel)
		}
		span.End(&err)
	}()
	flags := flag.NewFlagSet("nga-reminder", flag.ContinueOnError)
	flags.SetOutput(io.Discard)
	filename := flags.String("config", "", "JSON configuration file; omit to use defaults and environment variables")
	if err = flags.Parse(os.Args[1:]); errors.Is(err, flag.ErrHelp) {
		fmt.Fprintln(os.Stdout, "Usage: nga-reminder [-config config.json]\nOverride configuration with NGA_REMINDER_ environment variables; see service/docs/README.md.")
		err = nil
		return 0
	} else if err != nil {
		err = logging.Wrap(err, "parse command-line arguments")
		return 1
	}
	if flags.NArg() != 0 {
		err = logging.WithStack(errors.New("positional arguments are not supported; use -config to specify the configuration file"))
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
	monitor, err := service.NewMonitoring(ctx, cfg, store, infrastructure.NewNGA(cfg.NGAUserAgent, nil), log, infrastructure.NewNotifier(nil))
	if err != nil {
		return err
	}
	defer monitor.Close()
	router, err := handler.New(admin, monitor, log)
	if err != nil {
		return err
	}
	listener, err := net.Listen("tcp", cfg.ListenAddress)
	if err != nil {
		return logging.Wrap(err, "listen on HTTP address")
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
		result <- logging.Wrap(serveErr, "HTTP server exited")
	}()
	monitor.StartScheduler()
	zerolog.Ctx(ctx).Info().Str("event", "server_started").Str("listen_address", listener.Addr().String()).
		Bool("background_enabled", cfg.BackgroundEnabled).Msg("HTTP server started")
	select {
	case err = <-result:
		return err
	case <-ctx.Done():
		zerolog.Ctx(ctx).Info().Msg("Stopping new requests and waiting for active requests")
		shutdownCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		if err = server.Shutdown(shutdownCtx); err != nil {
			return errors.Join(logging.Wrap(err, "HTTP shutdown timed out"), logging.Wrap(server.Close(), "force-close HTTP server"))
		}
		return <-result
	}
}
