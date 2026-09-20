package infrastructure

import (
	"context"
	"crypto/tls"
	"net"
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"
)

func TestDownloadResourceUsesBrowserTLSFingerprint(t *testing.T) {
	const payload = "browser-tls-fixture"

	var (
		mu          sync.Mutex
		browserLike = make(map[string]bool)
	)
	server := httptest.NewUnstartedServer(http.HandlerFunc(func(response http.ResponseWriter, request *http.Request) {
		mu.Lock()
		allowed := browserLike[request.RemoteAddr]
		mu.Unlock()
		if !allowed {
			response.WriteHeader(567)
			return
		}
		response.Header().Set("Content-Type", "image/jpeg")
		_, _ = response.Write([]byte(payload))
	}))
	server.EnableHTTP2 = true
	server.TLS = &tls.Config{
		NextProtos: []string{"h2", "http/1.1"},
		GetConfigForClient: func(hello *tls.ClientHelloInfo) (*tls.Config, error) {
			allowed := hasGREASE(hello.CipherSuites) || hasGREASECurves(hello.SupportedCurves)
			mu.Lock()
			browserLike[hello.Conn.RemoteAddr().String()] = allowed
			mu.Unlock()
			return nil, nil
		},
	}
	server.StartTLS()
	defer server.Close()

	transport := server.Client().Transport.(*http.Transport).Clone()
	transport.TLSClientConfig.ServerName = "example.com"
	fixtureAddress := server.Listener.Addr().String()
	transport.DialContext = func(ctx context.Context, network, _ string) (net.Conn, error) {
		return (&net.Dialer{}).DialContext(ctx, network, fixtureAddress)
	}
	sender := NewNotifier(transport)
	defer sender.Close()

	data, mime, err := sender.DownloadResource(context.Background(), "https://img.nga.cn/browser-tls.jpg", 1<<20, false)
	if err != nil || string(data) != payload || mime != "image/jpeg" {
		t.Fatalf("browser-like resource download failed: data=%q mime=%q err=%v", data, mime, err)
	}
}

func hasGREASE(values []uint16) bool {
	for _, value := range values {
		if value&0x0f0f == 0x0a0a && byte(value>>8) == byte(value) {
			return true
		}
	}
	return false
}

func hasGREASECurves(values []tls.CurveID) bool {
	for _, value := range values {
		if hasGREASE([]uint16{uint16(value)}) {
			return true
		}
	}
	return false
}
