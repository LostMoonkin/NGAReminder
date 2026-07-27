CREATE TABLE nga_accounts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    label TEXT NOT NULL UNIQUE,
    passport_uid_encrypted BYTEA NOT NULL,
    passport_cid_encrypted BYTEA NOT NULL,
    encryption_version SMALLINT NOT NULL DEFAULT 1 CHECK (encryption_version > 0),
    status TEXT NOT NULL DEFAULT 'unchecked'
        CHECK (status IN ('unchecked', 'valid', 'invalid', 'paused')),
    last_auth_checked_at TIMESTAMPTZ,
    last_auth_error_kind TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

COMMENT ON TABLE nga_accounts IS
    'Encrypted NGA Passport credentials for the single-user service instance';
COMMENT ON COLUMN nga_accounts.passport_uid_encrypted IS
    'Versioned application-encrypted payload; never plaintext';
COMMENT ON COLUMN nga_accounts.passport_cid_encrypted IS
    'Versioned application-encrypted payload; never plaintext';
