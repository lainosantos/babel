#![forbid(unsafe_code)]

pub(crate) mod retention;
mod session;

pub mod audio;
pub mod autostart;
pub mod commands;
pub mod config;
pub mod credentials;
pub mod dashboard;
pub mod engine;
pub mod execution;
pub mod feedback;
pub mod history;
pub mod i18n;
pub mod interface_messages;
pub mod local_runtime;
pub mod logging;
pub mod mcp_client;
pub mod platform;
pub mod provider;
pub mod recording;
pub mod storage;
pub mod transcript;
pub mod tray;
