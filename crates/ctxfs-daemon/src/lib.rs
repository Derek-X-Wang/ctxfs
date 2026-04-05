// `tokio::select!` generates `_ = fut => {}` arms that clippy flags as
// "matching over ()". Detached `tokio::spawn` return values are idiomatic.
#![allow(clippy::ignored_unit_patterns, clippy::let_underscore_future)]

mod daemon;

pub use daemon::Daemon;
