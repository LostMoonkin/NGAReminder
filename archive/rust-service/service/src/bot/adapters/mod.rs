//! Platform bot adapters. Feishu is the first adapter; Telegram/QQ arrive in
//! a later phase and must pass the same adapter contract tests.

pub mod feishu;

pub use feishu::FeishuAdapter;
