//! Integration test: Cross-type interactions between core types.
//!
//! Verifies that `SourceSpec`, `Digest`, `Config`, and `CtxfsError`
//! work together correctly in realistic scenarios.

use ctxfs_core::config::Config;
use ctxfs_core::error::CtxfsError;
use ctxfs_core::source::{ProviderType, SourceSpec};
use ctxfs_core::Digest;

#[test]
fn source_spec_to_digest_to_path() {
    let source = SourceSpec::parse("github:octocat/Hello-World@main").unwrap();
    let key = format!("{source}:{}", source.id());

    // Hash the source identifier to produce a content-addressable key
    let digest = Digest::sha256(key.as_bytes());

    // Verify the path is well-formed
    let path = digest.to_path();
    assert!(path.starts_with("sha256/"));
    assert_eq!(path.components().count(), 3); // sha256 / prefix / hex
}

#[test]
fn config_defaults_produce_valid_paths() {
    let config = Config::default();

    // All paths should be under the same base directory
    let socket_parent = config.socket_path.parent().unwrap();
    let pid_parent = config.pid_file.parent().unwrap();
    let cache_parent = config.cache_dir.parent().unwrap();

    assert_eq!(socket_parent, pid_parent);
    assert_eq!(pid_parent, cache_parent);
}

#[test]
fn error_chain_preserves_context() {
    // Provider error wrapping
    let inner = "connection refused";
    let err = CtxfsError::Provider(format!("GitHub API: {inner}"));
    let msg = err.to_string();
    assert!(msg.contains("provider error"));
    assert!(msg.contains("connection refused"));

    // IO error conversion
    let io_err = std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "cannot read /etc/shadow",
    );
    let ctxfs_err = CtxfsError::from(io_err);
    assert!(ctxfs_err.to_string().contains("cannot read"));
}

#[test]
fn provider_type_roundtrips_through_source_spec() {
    let spec = SourceSpec::parse("github:owner/repo@v1.0").unwrap();
    assert_eq!(spec.provider_type, ProviderType::GitHub);

    // Serialize to JSON and back
    let json = serde_json::to_string(&spec).unwrap();
    let spec2: SourceSpec = serde_json::from_str(&json).unwrap();
    assert_eq!(spec2.provider_type, ProviderType::GitHub);

    // Display and re-parse
    let display = spec.to_string();
    let spec3 = SourceSpec::parse(&display).unwrap();
    assert_eq!(spec3, spec);
}

#[test]
fn source_spec_display_parse_roundtrip_all_forms() {
    let cases = [
        "github:octocat/Hello-World@master",
        "github:rust-lang/rust@1.75.0",
        "github:owner/repo@abc123:src/lib",
    ];

    for input in &cases {
        let parsed = SourceSpec::parse(input).unwrap();
        let displayed = parsed.to_string();
        let reparsed = SourceSpec::parse(&displayed).unwrap();
        assert_eq!(parsed, reparsed, "roundtrip failed for {input}");
    }
}

#[test]
fn digest_content_addressing_is_stable() {
    // The same content always produces the same digest
    let content = b"fn main() { println!(\"hello\"); }";

    let d1 = Digest::sha256(content);
    let d2 = Digest::sha256(content);
    assert_eq!(d1, d2);
    assert_eq!(d1.to_path(), d2.to_path());

    // Different content produces different digests
    let d3 = Digest::sha256(b"fn main() { println!(\"world\"); }");
    assert_ne!(d1, d3);
}

#[test]
fn config_serde_preserves_all_fields() {
    let config = Config {
        github_token: Some("ghp_test123".to_string()),
        log_level: "debug".to_string(),
        cache_max_bytes: 1024,
        ..Config::default()
    };

    let json = serde_json::to_string(&config).unwrap();
    let config2: Config = serde_json::from_str(&json).unwrap();

    assert_eq!(config.socket_path, config2.socket_path);
    assert_eq!(config.pid_file, config2.pid_file);
    assert_eq!(config.cache_dir, config2.cache_dir);
    assert_eq!(config.cache_max_bytes, config2.cache_max_bytes);
    assert_eq!(config.log_level, config2.log_level);
    assert_eq!(config.github_token, config2.github_token);
}
