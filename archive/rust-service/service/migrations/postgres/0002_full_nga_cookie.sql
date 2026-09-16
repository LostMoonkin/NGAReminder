ALTER TABLE nga_accounts
    ADD COLUMN cookie_encrypted BYTEA;

COMMENT ON COLUMN nga_accounts.cookie_encrypted IS
    'Optional encrypted full Cookie header for cross-user NGA searches; never plaintext';
