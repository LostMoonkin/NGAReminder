//! Platform integration model: a platform connection holds app credentials
//! while notification channels hold per-recipient targets. Bot capability is
//! owned by the connection, not by any single notification target.

pub mod feishu;
pub mod integration;
