//! Shared utilities for vdsm-rs: errors, config, logging.

pub mod config;
pub mod error;
pub mod logging;

pub const VDSM_RS_VERSION: &str = env!("CARGO_PKG_VERSION");
