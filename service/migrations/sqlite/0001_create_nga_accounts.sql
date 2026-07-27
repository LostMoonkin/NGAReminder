CREATE TABLE nga_accounts (
    id TEXT PRIMARY KEY NOT NULL DEFAULT (
        lower(hex(randomblob(4))) || '-' ||
        lower(hex(randomblob(2))) || '-4' ||
        substr(lower(hex(randomblob(2))), 2) || '-' ||
        substr('89ab', abs(random()) % 4 + 1, 1) ||
        substr(lower(hex(randomblob(2))), 2) || '-' ||
        lower(hex(randomblob(6)))
    ),
    label TEXT NOT NULL UNIQUE,
    passport_uid_encrypted BLOB NOT NULL,
    passport_cid_encrypted BLOB NOT NULL,
    encryption_version INTEGER NOT NULL DEFAULT 1 CHECK (encryption_version > 0),
    status TEXT NOT NULL DEFAULT 'unchecked'
        CHECK (status IN ('unchecked', 'valid', 'invalid', 'paused')),
    last_auth_checked_at TEXT,
    last_auth_error_kind TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
