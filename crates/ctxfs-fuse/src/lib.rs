// The FUSE backend is legacy — prefer `ctxfs-nfs` for new work. This crate
// still builds against macFUSE/libfuse for users who prefer it, but is no
// longer the primary code path, so we relax some pedantic lints here.
#![allow(
    clippy::unnecessary_self_imports,
    clippy::manual_let_else,
    clippy::manual_div_ceil,
    clippy::single_match_else,
    clippy::match_single_binding,
    clippy::cast_possible_wrap,
    clippy::unused_self
)]

mod fs;

pub use fs::CtxfsFilesystem;
