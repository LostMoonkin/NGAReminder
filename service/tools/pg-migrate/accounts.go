package main

import (
	"strings"

	"ngareminder/service/internal/infrastructure"
	"ngareminder/service/internal/repository"
)

func (m *importer) accounts() error {
	if m.report.Source["nga_accounts"] > 1 {
		return sourceError("nga_accounts", "id", "Go supports one account; resolve multiple accounts before migration")
	}
	return m.walk("nga_accounts", func(r row) error {
		m.accountID = r.text("id")
		uid, cid := m.decrypt(r, "passport_uid_encrypted", ""), m.decrypt(r, "passport_cid_encrypted", "")
		full := ""
		if len(r.blob("cookie_encrypted")) > 0 {
			full = m.decrypt(r, "cookie_encrypted", "")
		}
		creds, err := infrastructure.ParseCredentials(full, uid, cid)
		if err != nil {
			return sourceError(r.table, "cookie_encrypted", "invalid stored NGA credentials")
		}
		if creds.UID != strings.TrimSpace(uid) {
			return sourceError(r.table, "cookie_encrypted", "passport UID does not match full Cookie")
		}
		if full != "" {
			for _, p := range strings.Split(creds.Cookie, ";") {
				k, v, _ := strings.Cut(strings.TrimSpace(p), "=")
				if k == "ngaPassportCid" && v != strings.TrimSpace(cid) {
					return sourceError(r.table, "cookie_encrypted", "passport CID does not match full Cookie")
				}
			}
		}
		cookie, err := m.cipher.Encrypt(m.ctx, creds.Cookie)
		if err != nil {
			return err
		}
		check, err := m.cipher.Decrypt(m.ctx, cookie)
		if err != nil {
			return err
		}
		if check.Cookie != creds.Cookie {
			return sourceError(r.table, "cookie_encrypted", "target Cookie round-trip failed")
		}
		status := r.text("status")
		message := ""
		switch status {
		case "valid", "unchecked":
		case "invalid", "paused":
			status = "auth_paused"
			message = "迁移前账号不可用，请校验或更新 Cookie"
		default:
			return sourceError(r.table, "status", "unsupported account status")
		}
		mask := "***"
		if len(creds.UID) > 3 {
			mask += creds.UID[len(creds.UID)-3:]
		}
		return m.write(&repository.Account{ID: 1, Cookie: cookie, UIDMasked: mask, FullCookie: creds.FullCookie, Status: status, CheckedAt: r.timestamp("last_auth_checked_at"), LastError: message})
	})
}

func (m *importer) renewal() error {
	if err := m.walk("nga_account_renewal_settings", func(r row) error {
		id := r.text("account_id")
		if id != m.accountID {
			return sourceError(r.table, "account_id", "missing migrated account")
		}
		name := m.decrypt(r, "login_name_encrypted", "nga_account:"+id+":renewal_login:v2")
		password := m.decrypt(r, "password_encrypted", "nga_account:"+id+":renewal_password:v2")
		if name == "" || password == "" {
			return sourceError(r.table, "password_encrypted", "renewal credentials are empty")
		}
		binding := m.bindings[r.text("bot_binding_id")]
		enabled := r.yes("enabled")
		if binding == 0 && enabled {
			return sourceError(r.table, "bot_binding_id", "enabled renewal requires a migrated private owner binding")
		}
		status := r.text("credential_status")
		if status != "ready" {
			if status != "invalid" && status != "cooldown" {
				return sourceError(r.table, "credential_status", "unsupported renewal status")
			}
			enabled = false
			m.report.Converted["renewal_requires_manual_enable"]++
			m.report.Notes = append(m.report.Notes, "续期凭据原处于 invalid/cooldown，已保留凭据并关闭自动续期；校验后在管理页手动启用。")
		}
		return m.write(&repository.RenewalSettings{ID: 1, Enabled: enabled, BindingID: binding, Secret: m.seal("renewal-secret-v1", struct{ Name, Password string }{name, password})})
	}); err != nil {
		return err
	}
	return m.walk("nga_login_sessions", func(r row) error {
		if r.text("account_id") != m.accountID {
			return sourceError(r.table, "account_id", "missing migrated account")
		}
		status := r.text("status")
		message := ""
		switch status {
		case "awaiting_confirmation", "starting", "awaiting_captcha", "submitting", "validating_cookie":
			status = "interrupted"
			message = "迁移中断了旧登录流程，请重新发起续期"
			m.report.Converted["login_sessions_interrupted"]++
		case "succeeded", "cancelled", "expired":
		case "failed", "unsupported_challenge":
			status = "failed"
			message = "旧登录失败，详情保留在 PG 快照"
		default:
			return sourceError(r.table, "status", "unsupported login status")
		}
		return m.write(&repository.RenewalRequest{ID: r.text("id"), BindingID: m.bindings[r.text("bot_binding_id")], Status: status, Error: message, CreatedAt: r.at("created_at"), ExpiresAt: r.at("expires_at")})
	})
}
