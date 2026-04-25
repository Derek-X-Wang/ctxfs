// `tokio::select!` generates `_ = fut => {}` arms that clippy flags as
// "matching over ()". Detached `tokio::spawn` return values are idiomatic.
#![allow(clippy::ignored_unit_patterns, clippy::let_underscore_future)]

pub mod daemon;
pub mod fskit_mount;
pub mod mount_state;
pub mod observability;

pub use daemon::Daemon;
