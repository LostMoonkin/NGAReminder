package infrastructure

import (
	"context"
	"crypto/aes"
	"crypto/cipher"
	"crypto/rand"
	"encoding/base64"
	"errors"
	"strconv"
	"strings"

	"ngareminder/service/internal/logging"
)

var ErrCredentials = errors.New("请提供包含 ngaPassportUid 和 ngaPassportCid 的 Cookie，或填写 passport UID/CID")

type Credentials struct {
	UID        string
	Cookie     string
	FullCookie bool
}

func ParseCredentials(cookie, uid, cid string) (Credentials, error) {
	full := strings.TrimSpace(cookie) != ""
	if full {
		cookie = strings.TrimSpace(strings.TrimPrefix(strings.TrimSpace(cookie), "Cookie:"))
		uid, cid = "", ""
		parts := []string{}
		for _, part := range strings.Split(cookie, ";") {
			if strings.TrimSpace(part) == "" {
				continue
			}
			name, value, ok := strings.Cut(strings.TrimSpace(part), "=")
			name, value = strings.TrimSpace(name), strings.TrimSpace(value)
			if !ok || name == "" || value == "" {
				return Credentials{}, logging.WithStack(ErrCredentials)
			}
			switch name {
			case "ngaPassportUid":
				uid = value
			case "ngaPassportCid":
				cid = value
			}
			parts = append(parts, name+"="+value)
		}
		cookie = strings.Join(parts, "; ")
	} else {
		uid, cid = strings.TrimSpace(uid), strings.TrimSpace(cid)
		if strings.Contains(uid+cid, ";") {
			return Credentials{}, logging.WithStack(ErrCredentials)
		}
		cookie = "ngaPassportUid=" + uid + "; ngaPassportCid=" + cid
	}
	if _, err := strconv.ParseInt(uid, 10, 64); err != nil || strings.TrimSpace(cid) == "" {
		return Credentials{}, logging.WithStack(ErrCredentials)
	}
	// Cookie 是单行 HTTP header；错误中不能带上用户输入。
	for _, b := range []byte(cookie) {
		if b < 32 || b == 127 {
			return Credentials{}, logging.WithStack(ErrCredentials)
		}
	}
	return Credentials{uid, cookie, full}, nil
}

// 一个部署密钥加密完整 Cookie，随机 nonce 与密文一起保存；没有第二份明文 UID/CID。
type CredentialCipher struct{ aead cipher.AEAD }

func NewCredentialCipher(key string) (*CredentialCipher, error) {
	decoded, err := base64.StdEncoding.DecodeString(key)
	if err != nil || len(decoded) != 32 {
		return nil, logging.WithStack(errors.New("encryption_key 必须是标准 Base64 编码的 32 字节密钥"))
	}
	block, err := aes.NewCipher(decoded)
	if err != nil {
		return nil, logging.Wrap(err, "创建凭据加密器")
	}
	aead, err := cipher.NewGCM(block)
	if err != nil {
		return nil, logging.Wrap(err, "创建 AES-GCM")
	}
	return &CredentialCipher{aead}, nil
}

func (c *CredentialCipher) Encrypt(ctx context.Context, cookie string) (encrypted []byte, err error) {
	_, span := logging.Start(ctx, "infrastructure.encrypt_credentials")
	defer span.End(&err)
	nonce := make([]byte, c.aead.NonceSize())
	if _, err = rand.Read(nonce); err != nil {
		return nil, logging.Wrap(err, "生成凭据 nonce")
	}
	return c.aead.Seal(nonce, nonce, []byte(cookie), []byte("nga-account-v1")), nil
}

func (c *CredentialCipher) Decrypt(ctx context.Context, encrypted []byte) (credentials Credentials, err error) {
	_, span := logging.Start(ctx, "infrastructure.decrypt_credentials")
	defer span.End(&err)
	n := c.aead.NonceSize()
	if len(encrypted) < n {
		return credentials, logging.WithStack(errors.New("已保存的 NGA 凭据密文不完整"))
	}
	plain, err := c.aead.Open(nil, encrypted[:n], encrypted[n:], []byte("nga-account-v1"))
	if err != nil {
		return credentials, logging.Wrap(err, "解密 NGA 凭据失败，请检查部署密钥")
	}
	return ParseCredentials(string(plain), "", "")
}
