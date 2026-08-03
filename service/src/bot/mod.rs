//! Bot runtime: platform adapters, event normalization, authorization,
//! command dispatch and the reply outbox.

pub mod adapter;
pub mod adapters;
pub mod authorization;
pub mod commands;
pub mod dispatcher;
pub mod domain;
pub mod outbox;
pub mod parser;
pub mod repository;
pub mod runtime;
pub mod session;
