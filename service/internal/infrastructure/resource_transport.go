package infrastructure

import (
	"context"
	"crypto/tls"
	"fmt"
	"net"
	"net/http"
	"time"

	utls "github.com/refraction-networking/utls"
)

// browserResourceTransport gives NGA resource requests a browser-shaped
// ClientHello while keeping HTTP/1.1, which net/http can use on the returned
// uTLS connection. Notification APIs continue to use the standard transport.
func browserResourceTransport(base *http.Transport) *http.Transport {
	transport := base.Clone()
	dialContext := transport.DialContext
	if dialContext == nil {
		dialer := &net.Dialer{Timeout: 30 * time.Second, KeepAlive: 30 * time.Second}
		dialContext = dialer.DialContext
	}
	standardTLS := transport.TLSClientConfig

	transport.ForceAttemptHTTP2 = false
	transport.TLSNextProto = make(map[string]func(string, *tls.Conn) http.RoundTripper)
	transport.DialTLS = nil
	transport.DialTLSContext = func(ctx context.Context, network, address string) (net.Conn, error) {
		plain, err := dialContext(ctx, network, address)
		if err != nil {
			return nil, err
		}
		closeWithError := func(err error) (net.Conn, error) {
			_ = plain.Close()
			return nil, err
		}

		host, _, err := net.SplitHostPort(address)
		if err != nil {
			return closeWithError(fmt.Errorf("split TLS address: %w", err))
		}
		config := resourceTLSConfig(standardTLS, host)
		connection := utls.UClient(plain, config, utls.HelloCustom)
		spec, err := chromeHTTP1Spec()
		if err != nil {
			return closeWithError(fmt.Errorf("build browser TLS profile: %w", err))
		}
		if err = connection.ApplyPreset(&spec); err != nil {
			return closeWithError(fmt.Errorf("apply browser TLS profile: %w", err))
		}
		if err = connection.HandshakeContext(ctx); err != nil {
			return closeWithError(err)
		}
		return connection, nil
	}
	return transport
}

func resourceTLSConfig(base *tls.Config, host string) *utls.Config {
	if base == nil {
		return &utls.Config{ServerName: host, NextProtos: []string{"http/1.1"}}
	}
	serverName := base.ServerName
	if serverName == "" {
		serverName = host
	}
	return &utls.Config{
		Rand:                        base.Rand,
		Time:                        base.Time,
		RootCAs:                     base.RootCAs,
		NextProtos:                  []string{"http/1.1"},
		ServerName:                  serverName,
		InsecureSkipVerify:          base.InsecureSkipVerify,
		VerifyPeerCertificate:       base.VerifyPeerCertificate,
		CipherSuites:                base.CipherSuites,
		SessionTicketsDisabled:      base.SessionTicketsDisabled,
		MinVersion:                  base.MinVersion,
		MaxVersion:                  base.MaxVersion,
		DynamicRecordSizingDisabled: base.DynamicRecordSizingDisabled,
		KeyLogWriter:                base.KeyLogWriter,
		Renegotiation:               utls.RenegotiationSupport(base.Renegotiation),
	}
}

func chromeHTTP1Spec() (utls.ClientHelloSpec, error) {
	spec, err := utls.UTLSIdToSpec(utls.HelloChrome_Auto)
	if err != nil {
		return utls.ClientHelloSpec{}, err
	}
	extensions := make([]utls.TLSExtension, 0, len(spec.Extensions))
	for _, extension := range spec.Extensions {
		switch value := extension.(type) {
		case *utls.ALPNExtension:
			value.AlpnProtocols = []string{"http/1.1"}
			extensions = append(extensions, value)
		case *utls.ApplicationSettingsExtension, *utls.ApplicationSettingsExtensionNew:
			// ALPS only applies to HTTP/2; advertising it with HTTP/1.1 is invalid.
		default:
			extensions = append(extensions, extension)
		}
	}
	spec.Extensions = extensions
	return spec, nil
}
