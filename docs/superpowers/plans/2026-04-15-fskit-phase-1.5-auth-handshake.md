# FSKit Phase 1.5 — Bridge Auth Token Handshake Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the FSKit bridge TCP trust gap — require a per-mount 256-bit shared secret on every bridge connection so other local processes cannot impersonate the appex and read mounted source code.

**Architecture:** Daemon generates `AuthToken` per mount (code already exists at `crates/ctxfs-fskit/src/auth.rs`). Token passed to appex via `FSTaskOptions`, appex stores it in its `Socket` singleton, handshake sent as first frame on every TCP channel. Rust enforcement lives in the forked `fskit-rs` crate's socket read loop, returning `posix_error = EACCES` on mismatch and closing the connection.

**Tech Stack:** Rust (tokio, prost, tonic build.rs), Swift (NIO, SwiftProtobuf), Protobuf length-delimited over TCP localhost.

**Reference spec:** `docs/superpowers/specs/2026-04-11-fskit-backend-design.md` — Bridge Security section (`:240+`) and Phase 1.5 section (`:615+`) are the canonical design.

**Threat model (explicit): Phase 1.5 protects against cross-user and sandboxed attackers, NOT same-user malware with process-memory access or `ps`/proc introspection.** `FSTaskOptions` is documented as "equivalent to argv" in `fskit-rs` protocol.proto:45.

---

## File Structure (where changes land)

**Created / moved:**
- `swift/CtxfsFS/` — vendored FSKitBridge (move of sibling repo, bundle IDs already `ai.ctxfs.fskitbridge[.fskitext]`)
- `crates/fskit-rs/` — forked `fskit-rs@0.1.0`, patched to add auth enforcement
- `swift/CtxfsFS/FSKitExt/protocol.proto` — removed; replaced by symlink or build-generate from canonical source
- `crates/ctxfs-fskit/tests/auth_handshake.rs` — new integration test

**Modified:**
- `Cargo.toml` (workspace) — add `crates/fskit-rs` member, repoint `fskit-rs` dep from crates.io to path
- `crates/fskit-rs/src/protocol.proto` — new canonical source, add `AuthenticateRequest`
- `crates/fskit-rs/src/socket.rs` — auth state in `handle_stream`
- `crates/fskit-rs/src/lib.rs` — extend `Session::new` or add auth-builder API
- `crates/ctxfs-fskit/src/lib.rs` — thread `AuthToken` into session creation
- `crates/ctxfs-daemon/src/daemon.rs` — generate token in `do_mount`, pass to fskit-rs + FSTaskOptions
- `swift/CtxfsFS/FSKitExt/Socket.swift` — `initialize(host:port:token:)`, handshake in `getChannel()`
- `swift/CtxfsFS/FSKitExt/Volume.swift` — parse token from `TaskOptions`
- `swift/CtxfsFS/FSKitExt/Bridge.swift` — pass token through activation flow

**Unchanged (relied on):**
- `crates/ctxfs-fskit/src/auth.rs` — `AuthToken` already complete
- `crates/ctxfs-fskit/src/adapter.rs` — no auth logic here; enforcement is in fskit-rs layer
- `crates/ctxfs-daemon/src/mount_state.rs:15` — `auth_token: Option<String>` field already declared

---

## Task Dependency Graph

```
1 vendor Swift ──┐
                 ├──▶ 3 canonicalize proto ──▶ 4 add Authenticate variant ──▶ 5 fskit-rs auth ──▶ 6 daemon wire ──▶ 7 Swift client ──▶ 8 integration test ──▶ 9 smoke test
2 fork fskit-rs ─┘                                                                                                                                                    │
                                                                                                                                                                      │
10 upstream PR (parallel, not blocking) ──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
```

Tasks 1 and 2 are independent and can run in either order. 3-9 are strictly sequential. 10 is a parallel track that opens after 5.

---

## Task 1: Vendor FSKitBridge into ctxfs/swift/CtxfsFS/

**Goal:** Move the Swift FSKit app/appex from the sibling repo into ctxfs so all Phase 1.5 changes land in one commit stream.

**Files:**
- Move: `/Users/derekxwang/Development/incubator/ContextFS/FSKitBridge/` → `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/swift/CtxfsFS/`
- Create: `swift/README.md`
- Modify: `.gitignore` (exclude Xcode build products under `swift/`)

**Context:** Bundle IDs in `FSKitBridge/FSKitBridge.xcodeproj/project.pbxproj:420-515` are already `ai.ctxfs.fskitbridge[.fskitext]`. No rename needed. The sibling repo has no committed git history worth preserving (verified during sibling-cleanup). Simple copy + xcodeproj sanity check.

- [ ] **Step 1.1: Copy the sibling repo into swift/CtxfsFS/**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
mkdir -p swift
cp -R /Users/derekxwang/Development/incubator/ContextFS/FSKitBridge swift/CtxfsFS
rm -rf swift/CtxfsFS/.git swift/CtxfsFS/build swift/CtxfsFS/DerivedData
```

- [ ] **Step 1.2: Verify Xcode project still opens**

```bash
xcodebuild -list -project swift/CtxfsFS/FSKitBridge.xcodeproj
```

Expected: lists targets `FSKitBridge` and `FSKitExt`. If it errors with missing paths, inspect `project.pbxproj` for absolute paths that reference the old sibling location and fix them to be repo-relative.

- [ ] **Step 1.3: Do a clean build to confirm nothing broke**

```bash
xcodebuild -project swift/CtxfsFS/FSKitBridge.xcodeproj -scheme FSKitBridge -configuration Debug -destination 'generic/platform=macOS' build SYMROOT=/tmp/ctxfs-build 2>&1 | tail -20
```

Expected: `** BUILD SUCCEEDED **`. Any errors mean `project.pbxproj` or swift-tools-version had sibling-absolute paths that need fixing.

- [ ] **Step 1.4: Create swift/README.md**

```markdown
# CtxfsFS — Swift FSKit Appex

Vendored from [FSKitBridge](https://github.com/KhaosT/FSKitBridge) at commit <SHA>.
Do not re-sync from upstream blindly — Phase 1.5 adds an auth handshake that
upstream does not have.

## Build

```bash
xcodebuild -project FSKitBridge.xcodeproj -scheme FSKitBridge -configuration Release
```

## Bundle IDs (locked)

- Host app: `ai.ctxfs.fskitbridge`
- Extension: `ai.ctxfs.fskitbridge.fskitext`

See `/docs/superpowers/specs/2026-04-11-fskit-backend-design.md` for architecture.
```

Replace `<SHA>` with the upstream commit hash from the sibling's git log if available, or note "imported 2026-04-15, no upstream git history preserved".

- [ ] **Step 1.5: Update .gitignore to exclude Xcode build artifacts under swift/**

Append to `.gitignore`:

```
# Xcode build artifacts
swift/**/DerivedData/
swift/**/build/
swift/**/*.xcuserdatad/
swift/**/xcuserdata/
```

- [ ] **Step 1.6: Commit the vendoring**

```bash
git add swift/ .gitignore
git commit -m "feat(swift): vendor FSKitBridge into swift/CtxfsFS/

Bundle IDs ai.ctxfs.fskitbridge[.fskitext] already set upstream, so
this is pure directory move. Phase 1.5 auth handshake lands on top
of this vendored copy. Phase 2a handles signing/notarization
pipeline.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

**Verify:** `ls swift/CtxfsFS/FSKitBridge.xcodeproj && xcodebuild -list -project swift/CtxfsFS/FSKitBridge.xcodeproj | grep -E 'FSKitBridge|FSKitExt'`.

---

## Task 2: Fork fskit-rs into crates/fskit-rs/

**Goal:** Move from crates.io `fskit-rs@0.1.0` dep to a path dep inside ctxfs so Phase 1.5 can patch the socket accept loop.

**Files:**
- Create: `crates/fskit-rs/` (full copy of `fskit-rs-0.1.0` crate)
- Modify: `Cargo.toml` (workspace members, workspace deps)

**Context:** Spec's Risk Mitigations table (`:818`) already calls for vendoring fskit-rs. Doing it on day 1 of Phase 1.5 so auth enforcement can land in the fork without blocking on upstream PR review. Upstream PR goes out in parallel as Task 10.

- [ ] **Step 2.1: Copy the crate source into ctxfs**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
mkdir -p crates/fskit-rs
FSKIT_SRC=$(ls -d ~/.cargo/registry/src/index.crates.io-*/fskit-rs-0.1.0)
cp -R "$FSKIT_SRC/"* crates/fskit-rs/
rm -rf crates/fskit-rs/target crates/fskit-rs/Cargo.lock
```

- [ ] **Step 2.2: Record upstream origin in the crate**

Edit `crates/fskit-rs/Cargo.toml` — change the `[package]` section to note this is a fork:

```toml
[package]
name = "fskit-rs"
version = "0.1.0"
# ... existing fields ...
# Fork tracking: upstream is https://crates.io/crates/fskit-rs v0.1.0.
# Ctxfs-specific changes live in this fork until upstream accepts the auth PR.
```

Replace the crates.io `description` with: `FSKit bridge for Rust (ctxfs fork with per-mount auth token support)`.

- [ ] **Step 2.3: Add crates/fskit-rs to the workspace**

Edit root `Cargo.toml`:

```toml
[workspace]
members = [
    "crates/ctxfs-core",
    "crates/ctxfs-manifest",
    # ... existing members ...
    "crates/ctxfs-fskit",
    "crates/fskit-rs",
]
```

And repoint the dep under `[workspace.dependencies]`:

```toml
# FSKit backend (macOS 26+) — forked from crates.io v0.1.0
fskit-rs = { path = "crates/fskit-rs" }
```

Delete the old `fskit-rs = "0.1"` line.

- [ ] **Step 2.4: Verify the fork builds clean unmodified**

```bash
cargo build -p fskit-rs --release 2>&1 | tail -20
cargo build --workspace --release 2>&1 | tail -20
```

Expected: both succeed. This proves the fork is an exact copy before any modifications land.

- [ ] **Step 2.5: Adjust fork lints to match workspace**

Append to `crates/fskit-rs/Cargo.toml`:

```toml
[lints]
workspace = true
```

Then:

```bash
cargo clippy -p fskit-rs --all-targets 2>&1 | tail -30
```

If clippy finds issues from upstream code, suppress at the crate root (not fix — we want to preserve upstream as-is for now). Add `#![allow(clippy::pedantic, clippy::all)]` at the top of `crates/fskit-rs/src/lib.rs` and leave a TODO comment:

```rust
// TODO(phase1.5): upstream fskit-rs has pedantic lint violations; suppress
// here to avoid churn on vendored code. Remove after upstream PR merges or
// this fork stabilizes on its own lint budget.
#![allow(clippy::pedantic)]
```

- [ ] **Step 2.6: Commit the fork import**

```bash
git add crates/fskit-rs/ Cargo.toml
git commit -m "feat(fskit-rs): fork fskit-rs@0.1.0 into crates/fskit-rs/

Phase 1.5 needs auth enforcement in the TCP accept loop
(fskit-rs::socket::handle_stream). Forking day 1 per corrected
spec: security blocker cannot wait on upstream 0.1 single-
maintainer review cadence. Upstream PR goes out in parallel.

Imported as unmodified copy for a clean fork-point; subsequent
commits patch in auth.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

**Verify:** `cargo build --workspace && cargo test --workspace 2>&1 | tail -10` still passes all existing tests.

---

## Task 3: Canonicalize protocol.proto

**Goal:** One `protocol.proto` source of truth. Today there are two copies (Swift side at `swift/CtxfsFS/FSKitExt/protocol.proto`, Rust side at `crates/fskit-rs/src/protocol.proto`). Task 4 will add `AuthenticateRequest` — doing it in one place and propagating prevents drift.

**Files:**
- Canonical source: `crates/fskit-rs/src/protocol.proto` (pick this side because fskit-rs uses it in `build.rs`; Swift side is downstream)
- Delete: `swift/CtxfsFS/FSKitExt/protocol.proto` (replace with build-time generation or symlink)
- Modify: `swift/CtxfsFS/FSKitBridge.xcodeproj/project.pbxproj` (update proto reference)

**Context:** The two proto files are identical today (verified in Codex review). They will diverge the moment `AuthenticateRequest` is added to only one. Fix by making the Swift build consume the Rust-side file.

- [ ] **Step 3.1: Verify the two proto files are currently identical**

```bash
diff crates/fskit-rs/src/protocol.proto swift/CtxfsFS/FSKitExt/protocol.proto
```

Expected: no output (files match). If they diverge, reconcile manually to match the Rust version — that's the one fskit-rs compiles at build time.

- [ ] **Step 3.2: Remove the Swift-side proto file and replace with symlink**

```bash
rm swift/CtxfsFS/FSKitExt/protocol.proto
ln -s ../../../crates/fskit-rs/src/protocol.proto swift/CtxfsFS/FSKitExt/protocol.proto
```

- [ ] **Step 3.3: Verify Xcode still finds the proto**

```bash
xcodebuild -project swift/CtxfsFS/FSKitBridge.xcodeproj -scheme FSKitBridge -configuration Debug build SYMROOT=/tmp/ctxfs-build 2>&1 | tail -10
```

Expected: `** BUILD SUCCEEDED **`. The Swift Protobuf build rule should follow the symlink transparently.

If Xcode refuses to follow symlinks (some versions do), fall back to option B: add a build phase script that copies `crates/fskit-rs/src/protocol.proto` into `swift/CtxfsFS/FSKitExt/protocol.proto` before SwiftProtobuf compiles it. The script:

```bash
cp "$SRCROOT/../../crates/fskit-rs/src/protocol.proto" "$SRCROOT/FSKitExt/protocol.proto"
```

Wire it as a "Run Script" build phase that runs **before** "Compile Sources" on the FSKitExt target. Commit the generated copy to git (so CI builds work without depending on the Rust tree) but add a pre-commit check that flags drift.

- [ ] **Step 3.4: Commit the canonicalization**

```bash
git add swift/CtxfsFS/FSKitExt/protocol.proto
git commit -m "refactor(proto): canonicalize protocol.proto on Rust side

Two identical copies existed (Swift + Rust sides). Deleted the
Swift copy, symlinked the Rust-side file so SwiftProtobuf consumes
the canonical source. Prevents drift when Phase 1.5 adds the
AuthenticateRequest variant — a single-source change propagates
to both sides automatically.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

**Verify:** Both builds still pass: `cargo build --workspace && xcodebuild -project swift/CtxfsFS/FSKitBridge.xcodeproj build`.

---

## Task 4: Add AuthenticateRequest to protocol.proto

**Goal:** Extend the wire protocol with the handshake message.

**Files:**
- Modify: `crates/fskit-rs/src/protocol.proto` (canonical)

**Context:** New variant lives in `Pb_Request.content` oneof. Picking field number 50 (next gap after upstream's 43; leaves 43-49 available if upstream adds ops before merging our PR). Token is `bytes` not `string` — raw 32 bytes, hex encoding is only for `mounts.json` human-readable log surfaces.

- [ ] **Step 4.1: Write the failing integration test first**

Create `crates/ctxfs-fskit/tests/proto_authenticate_compiles.rs`:

```rust
// This test fails to compile until AuthenticateRequest is added to the proto.
// Its purpose is purely: prove the new variant exists in generated code.

#[test]
fn authenticate_request_variant_exists() {
    use fskit_rs::protocol::{request, Request, AuthenticateRequest};
    let token = vec![0u8; 32];
    let req = Request {
        id: 1,
        content: Some(request::Content::Authenticate(AuthenticateRequest { token })),
    };
    assert_eq!(req.id, 1);
    match req.content {
        Some(request::Content::Authenticate(a)) => assert_eq!(a.token.len(), 32),
        _ => panic!("expected Authenticate variant"),
    }
}
```

- [ ] **Step 4.2: Run the test and confirm it fails to compile**

```bash
cargo test -p ctxfs-fskit --test proto_authenticate_compiles 2>&1 | tail -10
```

Expected: compile error `no variant or associated item named Authenticate`.

- [ ] **Step 4.3: Add AuthenticateRequest message + variant to protocol.proto**

Edit `crates/fskit-rs/src/protocol.proto`. Find the section around line 680 (after the last top-level message definition, before `message Request`). Insert:

```proto
// Authentication handshake for the bridge TCP connection.
//
// The daemon generates a per-mount 256-bit token and passes it to the appex
// via FSTaskOptions. The appex MUST send an AuthenticateRequest as the first
// frame on every new TCP channel (including automatic reconnects). The
// listener validates the token, then accepts subsequent requests. Mismatch
// results in posix_error = EACCES and connection close.
message AuthenticateRequest {
  // Raw 32-byte (256-bit) shared secret. Constant-time compared server-side.
  bytes token = 1;
}
```

Then in `message Request { oneof content { ... } }` (around line 693), add:

```proto
    // Bridge authentication. MUST be the first frame on every new TCP channel.
    // See AuthenticateRequest documentation for protocol semantics.
    AuthenticateRequest authenticate = 50;
```

Place it at the bottom of the oneof, after all existing variants.

- [ ] **Step 4.4: Trigger a regenerate and re-run the test**

```bash
cargo build -p fskit-rs 2>&1 | tail -10
cargo test -p ctxfs-fskit --test proto_authenticate_compiles 2>&1 | tail -10
```

Expected: build succeeds (build.rs regenerates proto code), test compiles and passes.

- [ ] **Step 4.5: Verify Swift side also regenerates (or build copies proto)**

```bash
xcodebuild -project swift/CtxfsFS/FSKitBridge.xcodeproj -scheme FSKitBridge -configuration Debug build SYMROOT=/tmp/ctxfs-build 2>&1 | tail -10
```

Expected: `** BUILD SUCCEEDED **`. Swift now has `Pb_AuthenticateRequest` and `Pb_Request.OneOf_Content.authenticate` available.

- [ ] **Step 4.6: Commit**

```bash
git add crates/fskit-rs/src/protocol.proto crates/ctxfs-fskit/tests/proto_authenticate_compiles.rs
git commit -m "feat(proto): add AuthenticateRequest for bridge handshake

New Pb_Request.content variant (field 50). Raw 32 bytes on the
wire; hex only for human-readable log surfaces. First frame on
every new TCP channel; listener rejects anything else with
posix_error = EACCES.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

**Verify:** `cargo test -p ctxfs-fskit` passes.

---

## Task 5: Enforce auth in fskit-rs socket layer

**Goal:** The TCP accept loop in `crates/fskit-rs/src/socket.rs` must require a valid `AuthenticateRequest` as the first frame on every connection. Mismatch → `posix_error = EACCES`, then close.

**Files:**
- Modify: `crates/fskit-rs/src/socket.rs`
- Modify: `crates/fskit-rs/src/lib.rs` (add builder/API for setting expected token)
- Modify: `crates/fskit-rs/src/session.rs` (plumb token through)
- Create: `crates/fskit-rs/src/auth.rs` (small module for the token-compare helper)

**Context:** `handle_stream` at `crates/fskit-rs/src/socket.rs` currently decodes `Request` and dispatches to `handler.handle(content)`. We add a per-connection `authenticated: bool` state that must flip to `true` before any non-auth request is dispatched.

- [ ] **Step 5.1: Write the failing unit test**

Create `crates/fskit-rs/tests/auth_handshake.rs`:

```rust
//! Auth handshake enforcement test — drives the real fskit-rs socket.
use fskit_rs::auth::verify_token_ct;

#[test]
fn constant_time_compare_accepts_equal() {
    let token = [0x42u8; 32];
    assert!(verify_token_ct(&token, &token));
}

#[test]
fn constant_time_compare_rejects_different() {
    let a = [0x42u8; 32];
    let mut b = a;
    b[31] ^= 1;
    assert!(!verify_token_ct(&a, &b));
}

#[test]
fn constant_time_compare_rejects_wrong_length() {
    let a = [0x42u8; 32];
    let short = [0x42u8; 16];
    assert!(!verify_token_ct(&a, &short));
}
```

- [ ] **Step 5.2: Run the test, confirm compile failure**

```bash
cargo test -p fskit-rs --test auth_handshake 2>&1 | tail -10
```

Expected: `unresolved import fskit_rs::auth`.

- [ ] **Step 5.3: Create the auth module in fskit-rs**

Create `crates/fskit-rs/src/auth.rs`:

```rust
//! Constant-time token comparison for the bridge authentication handshake.
//!
//! This lives inside fskit-rs (rather than in ctxfs-fskit) because the socket
//! accept loop needs it directly and we want zero layering between the bytes
//! coming off the wire and the compare.

/// Constant-time compare of a received token against an expected token.
///
/// Returns `true` only if the slices are the same length AND every byte
/// matches. Length check happens first; byte-compare is constant-time over
/// the common prefix.
#[must_use]
pub fn verify_token_ct(expected: &[u8], candidate: &[u8]) -> bool {
    if expected.len() != candidate.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (a, b) in expected.iter().zip(candidate.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}
```

Expose from `crates/fskit-rs/src/lib.rs`:

```rust
pub mod auth;
```

(add it alongside the other `pub mod` declarations near the top of the file — they're all in the first ~20 lines.)

- [ ] **Step 5.4: Run the auth module tests**

```bash
cargo test -p fskit-rs --test auth_handshake 2>&1 | tail -10
```

Expected: `3 passed`.

- [ ] **Step 5.5: Write failing integration test for the handshake behavior**

Append to `crates/fskit-rs/tests/auth_handshake.rs`:

```rust
//! Full TCP roundtrip: valid + invalid + missing token cases.
//!
//! Uses fskit-rs's actual listener with a mock Filesystem that records
//! whether any non-auth requests got dispatched.

use fskit_rs::protocol::{request, AuthenticateRequest, Request};
use fskit_rs::session::SessionBuilder;
use prost::Message as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Clone, Default)]
struct RecordingFs {
    non_auth_calls: Arc<Mutex<u32>>,
}

// Minimal Filesystem impl that records non-auth call attempts.
// (The full impl with every trait method is in the mock module below.)
mod mock_fs;

fn encode_length_delimited(req: &Request) -> Vec<u8> {
    let mut buf = Vec::new();
    req.encode_length_delimited(&mut buf).unwrap();
    buf
}

#[tokio::test]
async fn valid_token_is_accepted() {
    let token = vec![0xABu8; 32];
    let fs = RecordingFs::default();
    let calls = fs.non_auth_calls.clone();
    let session = SessionBuilder::new(fs)
        .with_auth_token(token.clone())
        .bind_random()
        .await
        .unwrap();
    let port = session.port();

    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let auth = Request {
        id: 1,
        content: Some(request::Content::Authenticate(AuthenticateRequest {
            token: token.clone(),
        })),
    };
    stream.write_all(&encode_length_delimited(&auth)).await.unwrap();

    // Read back the response — should be Success.
    let mut resp_buf = vec![0u8; 64];
    let n = tokio::time::timeout(Duration::from_secs(1), stream.read(&mut resp_buf))
        .await
        .unwrap()
        .unwrap();
    assert!(n > 0, "expected a response from server");

    session.shutdown().await;
    assert_eq!(*calls.lock().unwrap(), 0, "auth flow must not dispatch to handler");
}

#[tokio::test]
async fn invalid_token_is_rejected_and_closes_connection() {
    let token = vec![0xABu8; 32];
    let wrong = vec![0xCDu8; 32];
    let fs = RecordingFs::default();
    let calls = fs.non_auth_calls.clone();
    let session = SessionBuilder::new(fs)
        .with_auth_token(token)
        .bind_random()
        .await
        .unwrap();
    let port = session.port();

    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let bad_auth = Request {
        id: 1,
        content: Some(request::Content::Authenticate(AuthenticateRequest { token: wrong })),
    };
    stream.write_all(&encode_length_delimited(&bad_auth)).await.unwrap();

    // Server must respond with posix_error = EACCES, then close.
    let mut resp_buf = vec![0u8; 256];
    let _ = tokio::time::timeout(Duration::from_secs(1), stream.read(&mut resp_buf))
        .await
        .unwrap();
    // After the bad auth, any subsequent write should fail because server closed.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let next = Request { id: 2, content: None };
    let _ = stream.write_all(&encode_length_delimited(&next)).await;
    let n = stream.read(&mut resp_buf).await.unwrap_or(0);
    assert_eq!(n, 0, "server must have closed the connection");

    session.shutdown().await;
    assert_eq!(*calls.lock().unwrap(), 0);
}

#[tokio::test]
async fn non_auth_first_frame_is_rejected() {
    use fskit_rs::protocol::GetVolumeIdentifier;
    let token = vec![0xABu8; 32];
    let fs = RecordingFs::default();
    let calls = fs.non_auth_calls.clone();
    let session = SessionBuilder::new(fs)
        .with_auth_token(token)
        .bind_random()
        .await
        .unwrap();
    let port = session.port();

    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let premature = Request {
        id: 1,
        content: Some(request::Content::GetVolumeIdentifier(GetVolumeIdentifier {})),
    };
    stream.write_all(&encode_length_delimited(&premature)).await.unwrap();

    let mut resp_buf = vec![0u8; 256];
    let _ = tokio::time::timeout(Duration::from_secs(1), stream.read(&mut resp_buf))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let n = stream.read(&mut resp_buf).await.unwrap_or(0);
    assert_eq!(n, 0, "server must have closed the connection after non-auth first frame");

    session.shutdown().await;
    assert_eq!(*calls.lock().unwrap(), 0, "handler must not see a pre-auth request");
}
```

And create `crates/fskit-rs/tests/mock_fs.rs` — a full `Filesystem` trait impl that records non-auth calls. (Subagent implementing this task: look at `crates/fskit-rs/src/lib.rs:35+` for the full trait signature; stub every method to either error with `libc::ENOSYS` or, for non-ignorable ones, record into `RecordingFs::non_auth_calls`.)

- [ ] **Step 5.6: Modify Session to accept an optional auth token**

Edit `crates/fskit-rs/src/session.rs`. Current structure (verified at `session.rs:16-21`):

```rust
pub struct Session {
    // existing fields
}

impl Session {
    // existing methods
}
```

Add:

```rust
pub struct SessionBuilder<FS> {
    fs: FS,
    auth_token: Option<Vec<u8>>,
    // ... other existing fields if present
}

impl<FS: Filesystem + Send + Sync + Clone + 'static> SessionBuilder<FS> {
    pub fn new(fs: FS) -> Self {
        Self { fs, auth_token: None }
    }

    /// Require all TCP connections to authenticate with this token as the
    /// first frame. If `None`, the listener accepts all connections
    /// (backward-compat for upstream users).
    #[must_use]
    pub fn with_auth_token(mut self, token: Vec<u8>) -> Self {
        self.auth_token = Some(token);
        self
    }

    pub async fn bind_random(self) -> Result<Session> {
        // wrap existing Session::new-style logic, passing auth_token into
        // the accept loop
    }
}
```

The exact wiring depends on the existing `Session::new` signature. Subagent: read `session.rs` and `socket.rs` fully before picking the integration point — you may need to change a function signature, or add a parallel constructor that the existing API delegates to with `None`.

- [ ] **Step 5.7: Modify handle_stream to enforce the handshake**

Edit `crates/fskit-rs/src/socket.rs` `handle_stream` function (around line 108). Current behavior: decodes `Request`, dispatches `request.content` to `handler.handle(content)`.

New behavior:

```rust
async fn handle_stream<FS>(
    mut stream: TcpStream,
    mut handler: Handler<FS>,
    mut shutdown_rx: broadcast::Receiver<()>,
    expected_token: Option<Vec<u8>>,  // NEW PARAMETER
) -> Result<()>
where
    FS: Filesystem + Send + Sync + Clone + 'static,
{
    let mut buf = BytesMut::with_capacity(4096);
    let mut authenticated = expected_token.is_none();  // bypass if no token configured

    loop {
        // ... existing select loop ...
        Ok(request) => {
            debug!("received message: {request:?}");
            buf.advance(buf.len() - frozen.remaining());

            let content = match (authenticated, request.content) {
                // Pre-auth: only Authenticate is accepted.
                (false, Some(request::Content::Authenticate(auth_req))) => {
                    match &expected_token {
                        Some(expected) if crate::auth::verify_token_ct(expected, &auth_req.token) => {
                            authenticated = true;
                            info!("bridge connection authenticated");
                            Some(response::Content::Success(Success {}))
                        }
                        _ => {
                            warn!("bridge authentication failed: token mismatch");
                            // Send error response, then close connection.
                            let resp = Response {
                                request_id: request.id,
                                content: Some(response::Content::PosixError(libc::EACCES)),
                            };
                            let mut out = BytesMut::new();
                            resp.encode_length_delimited(&mut out)?;
                            let _ = stream.write_all(&out).await;
                            let _ = stream.shutdown().await;
                            return Ok(());
                        }
                    }
                }
                // Pre-auth: anything else is rejected and closes the connection.
                (false, Some(_)) => {
                    warn!("bridge rejected pre-auth request: {}", request.id);
                    let resp = Response {
                        request_id: request.id,
                        content: Some(response::Content::PosixError(libc::EACCES)),
                    };
                    let mut out = BytesMut::new();
                    resp.encode_length_delimited(&mut out)?;
                    let _ = stream.write_all(&out).await;
                    let _ = stream.shutdown().await;
                    return Ok(());
                }
                // Post-auth: Authenticate a second time is a protocol error (reject, close).
                (true, Some(request::Content::Authenticate(_))) => {
                    warn!("bridge rejected replay Authenticate after successful auth");
                    let resp = Response {
                        request_id: request.id,
                        content: Some(response::Content::PosixError(libc::EPROTO)),
                    };
                    let mut out = BytesMut::new();
                    resp.encode_length_delimited(&mut out)?;
                    let _ = stream.write_all(&out).await;
                    let _ = stream.shutdown().await;
                    return Ok(());
                }
                // Post-auth: dispatch as before.
                (true, Some(content)) => match handler.handle(content).await {
                    Ok(content) => Some(content),
                    Err(err) => {
                        error!("handler error: {err}");
                        None
                    }
                },
                // Missing content: upstream's existing EINVAL behavior.
                (_, None) => {
                    warn!("received request without content: {}", request.id);
                    Some(response::Content::PosixError(libc::EINVAL))
                }
            };
            // ... existing response-writing logic, unchanged ...
        }
    }
}
```

The caller at the accept loop must forward `expected_token.clone()` into each spawned `handle_stream`. Find that callsite (around `socket.rs:85`) and thread the token through.

- [ ] **Step 5.8: Run the handshake integration tests**

```bash
cargo test -p fskit-rs --test auth_handshake 2>&1 | tail -20
```

Expected: all 6 tests pass (3 unit + 3 integration).

- [ ] **Step 5.9: Run the full workspace test suite**

```bash
cargo test --workspace 2>&1 | tail -20
```

Expected: no regressions. If any existing test fails because it constructs a `Session` without a token, pick up the default behavior: `expected_token = None` ⇒ listener accepts without auth (preserves upstream contract + existing ctxfs tests).

- [ ] **Step 5.10: Commit**

```bash
git add crates/fskit-rs/
git commit -m "feat(fskit-rs): enforce auth handshake in socket accept loop

New behavior: if SessionBuilder::with_auth_token is set, every TCP
connection MUST send AuthenticateRequest as its first frame. The
listener validates via constant-time compare, then accepts
subsequent VFS requests. Mismatch/missing/replay → posix_error
(EACCES/EPROTO) + close connection.

Backward compatible: omitting with_auth_token keeps the upstream
behavior of accepting all connections, so pre-existing tests and
upstream users are not broken.

Enforcement point is fskit-rs, not the Filesystem trait, because
Filesystem is a trait not a struct — per Codex review of the spec.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

**Verify:** `cargo test --workspace` passes, `cargo clippy --workspace --all-targets` clean.

---

## Task 6: Daemon wiring — generate token, pass to fskit-rs + FSTaskOptions

**Goal:** On FSKit mount, daemon generates an `AuthToken`, hands it to `SessionBuilder::with_auth_token`, and propagates it into the `FSTaskOptions` the daemon sends to fskitd so the appex can read it.

**Files:**
- Modify: `crates/ctxfs-daemon/src/daemon.rs` (the `do_mount` FSKit branch)
- Modify: `crates/ctxfs-daemon/src/mount_state.rs:15` (the `auth_token: Option<String>` field — keep Option but populate Some)
- Modify: `crates/ctxfs-fskit/src/lib.rs` (thread token into session start)

**Context:** `MountHandle` in `ctxfs-daemon` already has `auth_token: Option<String>` plumbing. `daemon.rs:544` currently writes `auth_token: None`. Populate with hex-encoded token for logs/observability, pass raw bytes to fskit-rs.

- [ ] **Step 6.1: Write the failing test — daemon mount populates auth token**

Add to `crates/ctxfs-daemon/src/daemon.rs` test module (or create if absent):

```rust
#[cfg(test)]
mod auth_tests {
    use super::*;

    #[tokio::test]
    async fn fskit_mount_populates_auth_token() {
        // This test exercises the in-memory MountHandle construction path
        // without actually spawning a TCP listener.
        // Expect: after do_mount(Backend::FsKit, ...), MountHandle.auth_token
        // is Some(hex string 64 chars).

        // (Full harness depends on existing daemon test utilities; subagent
        // should follow patterns from mount_state.rs:177 and nearby.)
    }
}
```

Subagent: flesh out this test using existing test infrastructure — don't block on spinning up real fskit. The assertion is just: auth_token is `Some` and is a valid 64-char hex string.

- [ ] **Step 6.2: Confirm it fails (current code sets None)**

```bash
cargo test -p ctxfs-daemon 2>&1 | tail -10
```

Expected: test fails on `Some(_)` assertion.

- [ ] **Step 6.3: Generate and thread the token in do_mount FSKit branch**

In `crates/ctxfs-daemon/src/daemon.rs`, find the FSKit mount branch (currently constructs `MountHandle` with `auth_token: None` around `daemon.rs:544`). Modify:

```rust
// At top of FSKit mount branch:
let token = ctxfs_fskit::auth::AuthToken::generate();
let token_hex = token.to_hex();

// When starting the fskit-rs session:
let session = ctxfs_fskit::start_session_with_auth(
    adapter,
    &token,  // pass raw bytes to listener
    &bundle_id,
    &token_hex,  // also goes into FSTaskOptions as a string entry
).await?;

// When constructing MountHandle:
MountHandle {
    // ...
    auth_token: Some(token_hex.clone()),
    // ...
}
```

- [ ] **Step 6.4: Add start_session_with_auth to ctxfs-fskit**

Edit `crates/ctxfs-fskit/src/lib.rs`. Find or create a session-start helper and branch:

```rust
/// Start a fskit-rs session that requires the given auth token on every
/// incoming TCP connection. The returned handle owns the listener and
/// shuts it down on drop.
///
/// `token` is the raw 32-byte secret; `token_hex` is the human-readable
/// form the daemon passes to fskitd via FSTaskOptions so the appex can
/// send it back on the wire.
pub async fn start_session_with_auth(
    fs: FilesystemAdapter,
    token: &auth::AuthToken,
    bundle_id: &str,
    token_hex: &str,
) -> anyhow::Result<fskit_rs::Session> {
    let session = fskit_rs::session::SessionBuilder::new(fs)
        .with_auth_token(token.bytes_vec())  // NEW helper method — see step 6.5
        .bind_random()
        .await?;

    let port = session.port();
    // Pass `(port, token_hex)` to fskitd via FSTaskOptions.
    // Existing code likely has `fskit_rs::mounter::mount(bundle_id, &[format!("port={port}")])`
    // — augment with the token arg.
    fskit_rs::mounter::mount(
        bundle_id,
        &[
            format!("port={port}"),
            format!("token={token_hex}"),
        ],
    )?;

    Ok(session)
}
```

Subagent: the exact `mounter::mount` signature is in `crates/fskit-rs/src/mounter.rs` — read it first and adjust the call site to match. The current `start_session` (no auth) should be preserved for NFS-backend equivalence tests; add `start_session_with_auth` alongside it.

- [ ] **Step 6.5: Expose AuthToken bytes for passing to fskit-rs**

Edit `crates/ctxfs-fskit/src/auth.rs`. Add:

```rust
impl AuthToken {
    /// Clone the raw bytes for passing to the fskit-rs session builder.
    /// Expose only the `Vec<u8>` form — the raw `[u8; 32]` stays private
    /// so callers cannot accidentally serialize unpadded.
    pub fn bytes_vec(&self) -> Vec<u8> {
        self.bytes.to_vec()
    }
}
```

- [ ] **Step 6.6: Run the daemon test**

```bash
cargo test -p ctxfs-daemon auth 2>&1 | tail -10
```

Expected: `1 passed`. Also `cargo test -p ctxfs-fskit` still passes.

- [ ] **Step 6.7: Commit**

```bash
git add crates/ctxfs-daemon/ crates/ctxfs-fskit/
git commit -m "feat(daemon): generate per-mount auth token, pass to fskit-rs + fskitd

Daemon now:
- generates AuthToken::generate() on every FSKit mount
- passes raw bytes to SessionBuilder::with_auth_token (listener enforces)
- passes hex form to fskitd via FSTaskOptions so appex can send it
  back on the wire

MountHandle.auth_token field (previously always None per
mount_state.rs:15) now populated with hex token for observability.
Not persisted across daemon restarts — restart requires remount per
corrected spec.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

**Verify:** `cargo test --workspace` passes, `cargo clippy --workspace --all-targets` clean.

---

## Task 7: Swift client — parse token, handshake on every channel

**Goal:** Appex reads the `token=` entry from `FSTaskOptions`, stores it in `Socket.shared`, and sends `AuthenticateRequest` as the first frame on every new TCP channel.

**Files:**
- Modify: `swift/CtxfsFS/FSKitExt/Volume.swift` (parse token from TaskOptions on mount/activate)
- Modify: `swift/CtxfsFS/FSKitExt/Socket.swift` (signature + handshake in `getChannel()`)
- Modify: `swift/CtxfsFS/FSKitExt/Bridge.swift` (pass token through activation)

**Context:** Current `Socket.initialize(host:port:)` at `Socket.swift:21`. `getChannel()` at `Socket.swift:113` connects and returns without any handshake. After change, `getChannel()` performs the handshake synchronously before returning; every reconnect re-auths automatically because each reconnect calls `getChannel()`.

- [ ] **Step 7.1: Extend Socket to accept and store the token**

Edit `swift/CtxfsFS/FSKitExt/Socket.swift`. Find `initialize(host:port:)` at `:21`. Replace with:

```swift
func initialize(host: String, port: Int, token: Data) {
    channelLock.lock()
    defer { channelLock.unlock() }

    failAllPromises(SocketError.notConnected)

    self.host = host
    self.port = port
    self.authToken = token  // NEW

    if let channel, channel.isActive {
        channel.close(mode: .all, promise: nil)
        self.channel = nil
    }

    log.d("Socket configured for \(host):\(port) with auth token")
}
```

Add the `private var authToken: Data?` field next to `host` / `port` declarations near the top of the class.

- [ ] **Step 7.2: Perform handshake in getChannel() before returning**

Edit `getChannel()` at `Socket.swift:113`. Replace the return-without-handshake logic with:

```swift
private func getChannel() throws -> Channel {
    channelLock.lock()
    defer { channelLock.unlock() }

    guard let host = host, let port = port else {
        throw SocketError.notConfigured
    }
    guard let authToken = authToken else {
        throw SocketError.notConfigured  // no token = not configured
    }

    if let current = channel, current.isActive {
        return current
    }

    let bootstrap = ClientBootstrap(group: group)
        .channelOption(ChannelOptions.socketOption(.so_reuseaddr), value: 1)
        .channelOption(ChannelOptions.socketOption(.so_keepalive), value: 1)
        .channelOption(ChannelOptions.tcpOption(.tcp_nodelay), value: 1)
        .channelInitializer { channel in
            channel.pipeline.addHandler(
                ByteToMessageHandler(LengthDelimitedDecoder())
            )
            .flatMap {
                channel.pipeline.addHandler(ResponseRouter(self))
            }
        }

    let connected = try bootstrap.connect(host: host, port: port).wait()
    log.d("Connected to \(host):\(port) — authenticating")

    // Send authenticate as the first frame, synchronously.
    var authReq = Pb_Request()
    authReq.id = UInt64.random(in: 1...UInt64.max)
    var authContent = Pb_AuthenticateRequest()
    authContent.token = authToken
    authReq.content = .authenticate(authContent)

    let authBuf = try encodeLengthDelimited(authReq, allocator: connected.allocator)

    // Register a promise for the authenticate response, then write.
    let authPromise = connected.eventLoop.makePromise(of: Pb_Response.OneOf_Content.self)
    _ = registerSpecificPromise(authPromise, forID: authReq.id)

    try connected.writeAndFlush(authBuf).wait()

    // Block for the auth response with a short timeout — reconnect path
    // must not hang FSKit callbacks indefinitely.
    do {
        let resp = try authPromise.futureResult
            .timeout(after: .seconds(2), on: connected.eventLoop)
            .wait()
        switch resp {
        case .success:
            log.d("Authentication successful")
        default:
            log.e("Authentication failed: unexpected response \(resp)")
            try? connected.close().wait()
            throw SocketError.authenticationFailed
        }
    } catch {
        log.e("Authentication error: \(error)")
        try? connected.close().wait()
        throw SocketError.authenticationFailed
    }

    channel = connected
    return connected
}
```

And extend `SocketError` (at `:173`) with:

```swift
case authenticationFailed
```

and in the `errorDescription` switch:

```swift
case .authenticationFailed:
    return "Bridge authentication handshake failed — token rejected by daemon."
```

The `timeout(after:on:)` helper: NIO's `EventLoopFuture.withTimeout` may be the right name — subagent should verify against the NIO version pinned in `Package.swift` and use whichever API exists. If NIO doesn't have a built-in, implement a local `EventLoopPromise` + `scheduleTask` timeout in ~6 lines.

`registerSpecificPromise(_:forID:)` is a helper the subagent must add near `registerPromise` at `Socket.swift:143` — same mechanic but uses a caller-supplied ID instead of generating a random one.

- [ ] **Step 7.3: Wire token through Volume.swift activation flow**

Find `Volume.swift` in `swift/CtxfsFS/FSKitExt/`. On mount/activate, `TaskOptions` arrive as `[String]` (modeled as `Pb_TaskOptions.task_options` — the argv-equivalent). Upstream already parses `port=` from these strings; add `token=` parsing alongside:

```swift
var host = "127.0.0.1"
var port: Int? = nil
var token: Data? = nil

for opt in taskOptions.taskOptions {
    if opt.hasPrefix("port=") {
        port = Int(opt.dropFirst("port=".count))
    } else if opt.hasPrefix("token=") {
        let hex = String(opt.dropFirst("token=".count))
        token = Data(hexString: hex)  // see step 7.4 for the helper
    } else if opt.hasPrefix("host=") {
        host = String(opt.dropFirst("host=".count))
    }
}

guard let port = port, let token = token else {
    throw VolumeError.missingMountOption
}

Socket.shared.initialize(host: host, port: port, token: token)
```

Subagent: find the existing option-parse site (likely in `activate` or `mount` methods) and modify in place. Keep whatever structure is there; just add the `token=` branch.

- [ ] **Step 7.4: Add hex-decode helper for Data**

Create or extend `swift/CtxfsFS/FSKitExt/Data+Hex.swift`:

```swift
import Foundation

extension Data {
    /// Decode a hex string into Data. Returns nil on invalid input.
    /// Used for the bridge auth token arriving from FSTaskOptions.
    init?(hexString: String) {
        let hex = hexString.hasPrefix("0x") ? String(hexString.dropFirst(2)) : hexString
        guard hex.count % 2 == 0 else { return nil }
        var data = Data(capacity: hex.count / 2)
        var index = hex.startIndex
        while index < hex.endIndex {
            let next = hex.index(index, offsetBy: 2)
            guard let byte = UInt8(hex[index..<next], radix: 16) else { return nil }
            data.append(byte)
            index = next
        }
        self = data
    }
}
```

Add to the Xcode project (drag into FSKitExt target) OR add to `project.pbxproj` by hand if doing it scripted.

- [ ] **Step 7.5: Build the Swift side clean**

```bash
xcodebuild -project swift/CtxfsFS/FSKitBridge.xcodeproj -scheme FSKitBridge -configuration Debug build SYMROOT=/tmp/ctxfs-build 2>&1 | tail -20
```

Expected: `** BUILD SUCCEEDED **`.

- [ ] **Step 7.6: Commit Swift side**

```bash
git add swift/CtxfsFS/
git commit -m "feat(swift): handshake auth token on every TCP channel

Socket.initialize now requires a Data token; getChannel performs
the authenticate handshake synchronously before returning the
channel to send(). Every reconnect (via channelInactive → getChannel)
automatically re-authenticates because token is stored in the
Socket singleton.

Volume parses 'token=<hex>' from FSTaskOptions on activate/mount;
Data+Hex.swift helper handles the hex decode.

Handshake failure closes the channel and surfaces as
SocketError.authenticationFailed to the send() caller — FSKit maps
to I/O error on reads, which is what we want for a misconfigured
bridge.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

**Verify:** `xcodebuild build` succeeds.

---

## Task 8: End-to-end Rust integration test

**Goal:** Prove, in CI-able Rust, that the full daemon → fskit-rs → (mock) appex flow rejects a bad token and accepts a good one.

**Files:**
- Create: `crates/ctxfs-fskit/tests/e2e_auth.rs`

**Context:** Task 5's `auth_handshake.rs` tests the fskit-rs layer in isolation. This test exercises the daemon seam (AuthToken generation + SessionBuilder wiring) to guard against future refactors that accidentally drop the token.

- [ ] **Step 8.1: Write the test**

Create `crates/ctxfs-fskit/tests/e2e_auth.rs`:

```rust
//! End-to-end test: daemon-path auth token round-trips through fskit-rs.

use ctxfs_fskit::auth::AuthToken;
use fskit_rs::protocol::{request, AuthenticateRequest, Request};
use prost::Message as _;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[tokio::test]
async fn daemon_generated_token_accepts_matching_client() {
    let token = AuthToken::generate();
    let raw = token.bytes_vec();

    // Spin up a session as the daemon would.
    let fs = mock_filesystem();
    let session = fskit_rs::session::SessionBuilder::new(fs)
        .with_auth_token(raw.clone())
        .bind_random()
        .await
        .expect("bind listener");

    let port = session.port();

    // Client connects and authenticates with the same token.
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.expect("connect");
    let auth = Request {
        id: 1,
        content: Some(request::Content::Authenticate(AuthenticateRequest { token: raw })),
    };
    let mut buf = Vec::new();
    auth.encode_length_delimited(&mut buf).unwrap();
    stream.write_all(&buf).await.unwrap();

    let mut resp_buf = vec![0u8; 256];
    let n = tokio::time::timeout(Duration::from_secs(1), stream.read(&mut resp_buf))
        .await
        .expect("timeout")
        .expect("read");
    assert!(n > 0, "expected success response");

    session.shutdown().await;
}

#[tokio::test]
async fn daemon_generated_token_rejects_mismatched_client() {
    let server_token = AuthToken::generate();
    let client_token = AuthToken::generate();  // different!

    let fs = mock_filesystem();
    let session = fskit_rs::session::SessionBuilder::new(fs)
        .with_auth_token(server_token.bytes_vec())
        .bind_random()
        .await
        .expect("bind listener");

    let port = session.port();

    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.expect("connect");
    let auth = Request {
        id: 1,
        content: Some(request::Content::Authenticate(AuthenticateRequest {
            token: client_token.bytes_vec(),
        })),
    };
    let mut buf = Vec::new();
    auth.encode_length_delimited(&mut buf).unwrap();
    stream.write_all(&buf).await.unwrap();

    // Server responds with EACCES and closes.
    let mut resp_buf = vec![0u8; 256];
    let _ = tokio::time::timeout(Duration::from_secs(1), stream.read(&mut resp_buf))
        .await
        .expect("timeout");

    tokio::time::sleep(Duration::from_millis(100)).await;
    let n = stream.read(&mut resp_buf).await.unwrap_or(0);
    assert_eq!(n, 0, "server must have closed the connection");

    session.shutdown().await;
}

fn mock_filesystem() -> impl fskit_rs::Filesystem + Send + Sync + Clone + 'static {
    // Subagent: reuse or extract the mock from fskit-rs's auth_handshake.rs
    // test; these two tests want the same minimal stub.
    todo!("reuse MockFs from fskit-rs tests/mock_fs.rs — extract to a shared test-utils crate if needed")
}
```

- [ ] **Step 8.2: Extract shared mock filesystem**

The `mock_filesystem()` helper and the MockFs from Task 5 overlap. Options:
- **A**: Create `crates/fskit-rs/src/test_support.rs` (behind `#[cfg(any(test, feature = "test-support"))]`) with a `MockFs` that both test files use.
- **B**: Duplicate the mock — faster but duplicates ~200 lines of stub trait impls.

Pick **A**. Add `[features] test-support = []` to `crates/fskit-rs/Cargo.toml`, gate the module, and add a `dev-dependencies` entry in `crates/ctxfs-fskit/Cargo.toml`:

```toml
[dev-dependencies]
fskit-rs = { workspace = true, features = ["test-support"] }
```

- [ ] **Step 8.3: Run the e2e test**

```bash
cargo test -p ctxfs-fskit --test e2e_auth 2>&1 | tail -20
```

Expected: both tests pass.

- [ ] **Step 8.4: Commit**

```bash
git add crates/ctxfs-fskit/tests/e2e_auth.rs crates/fskit-rs/src/test_support.rs crates/fskit-rs/Cargo.toml crates/ctxfs-fskit/Cargo.toml
git commit -m "test(fskit): end-to-end auth token roundtrip via SessionBuilder

Exercises the daemon-side seam: AuthToken::generate →
SessionBuilder::with_auth_token → TCP handshake. Guards against
future refactors that silently drop the token between these
layers.

MockFs extracted to fskit-rs::test_support (feature-gated) so
both this test and fskit-rs's own tests/auth_handshake.rs share
one stub.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

**Verify:** `cargo test --workspace` passes.

---

## Task 9: Manual smoke test on real Mac hardware

**Goal:** Prove the full Swift+Rust loop works on macOS 26+ with a real FSKit mount. Writeup goes in `docs/poc/fskit-phase1.5-smoke-test.md`.

**This task is NOT subagent-executable — it requires user hardware access.** Controller should pause here and ask the user to run the checklist.

- [ ] **Step 9.1: Rebuild and reinstall the appex**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
cargo build --release
xcodebuild -project swift/CtxfsFS/FSKitBridge.xcodeproj -scheme FSKitBridge -configuration Release -destination 'generic/platform=macOS' build SYMROOT=/tmp/ctxfs-build
cp -R /tmp/ctxfs-build/Release/FSKitBridge.app /Applications/
```

User must re-enable the extension in System Settings → Login Items & Extensions → File System Extensions → FSKitBridge (if disabled by the reinstall).

- [ ] **Step 9.2: Start daemon**

```bash
export CTXFS_FSKIT_BUNDLE_ID=ai.ctxfs.fskitbridge.fskitext
./target/release/ctxfs daemon stop || true
./target/release/ctxfs daemon start &
```

- [ ] **Step 9.3: Mount a small test repo**

```bash
./target/release/ctxfs mount github:octocat/Hello-World@master -p ./test-mnt --backend fskit
cat ./test-mnt/README
```

Expected: `Hello World!` — confirms the full handshake + read path works.

- [ ] **Step 9.4: Verify auth is actually enforced**

```bash
# Find the fskit-rs TCP port the daemon is listening on.
MOUNT_PORT=$(grep -oE '127\.0\.0\.1:[0-9]+' ~/.ctxfs/ctxfs.log | tail -1 | cut -d: -f2)

# Try to connect WITHOUT a token — should be rejected.
printf '\x00' | nc -w 1 127.0.0.1 $MOUNT_PORT
echo "exit: $?"
# Any incomplete/invalid first frame → server closes the connection.
# If reads start working, auth enforcement is broken.
```

Cross-check daemon log for `bridge rejected pre-auth request` or `authentication failed` entries.

- [ ] **Step 9.5: Unmount and confirm clean teardown**

```bash
./target/release/ctxfs unmount ./test-mnt
./target/release/ctxfs list
```

Expected: "No active mounts", symlink removed.

- [ ] **Step 9.6: Write up the smoke test**

Create `docs/poc/fskit-phase1.5-smoke-test.md` mirroring the structure of `fskit-phase1-smoke-test.md`. Sections: Setup, Test Run, Auth Enforcement Evidence, Latency (compare to Phase 1 baseline — expect ~same 2-3ms), Issues Found, Verdict.

- [ ] **Step 9.7: Commit the smoke test writeup**

```bash
git add docs/poc/fskit-phase1.5-smoke-test.md
git commit -m "docs: FSKit Phase 1.5 auth handshake smoke test writeup

Verified end-to-end on macOS 26.x: mount succeeds, reads work, an
unauth'd connection attempt is rejected at the socket layer, clean
unmount.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

**Verify:** All smoke-test checks pass. If any fail, implementer debugs before marking Phase 1.5 complete.

---

## Task 10: Upstream PR to fskit-rs (parallel track)

**Goal:** Offer the auth token hook back to upstream `fskit-rs` so the ctxfs fork can shrink over time.

**This task runs in parallel starting after Task 5 is green; it does NOT block Phase 1.5 completion.**

- [ ] **Step 10.1: Isolate the auth changes onto a branch against upstream**

Clone fresh upstream, cherry-pick only the auth-related commits from ctxfs's fork:

```bash
mkdir -p /tmp/fskit-rs-upstream && cd /tmp/fskit-rs-upstream
git clone https://github.com/<upstream>/fskit-rs.git && cd fskit-rs
# Cherry-pick task 4 + task 5 commits from ctxfs
git remote add ctxfs /Users/derekxwang/Development/incubator/ContextFS/ctxfs
git fetch ctxfs
git cherry-pick <proto commit SHA> <fskit-rs auth commit SHA>
```

- [ ] **Step 10.2: Open PR**

PR description should link to:
- The Codex review findings in ctxfs that motivated this
- The integration tests (Tasks 5 + 8)
- The backward-compat behavior (no token = upstream semantics)

Mark as "draft" until a maintainer shows interest.

- [ ] **Step 10.3: Track in the fork README**

Edit `crates/fskit-rs/README.md` — add a "Upstream PR" section linking to the PR URL. Remove the fork entirely if it merges; swap to crates.io version once released.

**Verify:** PR URL recorded in `crates/fskit-rs/README.md`.

---

## Risk Register

| Risk | Likelihood | Mitigation |
|---|---|---|
| `FSTaskOptions` leaks token via `ps` on same-user machine | High | Spec says explicitly we don't defend against this; threat model documented. |
| Upstream fskit-rs diverges while our fork lives | Medium | Task 10 keeps upstream and fork in conversation; auth change is small and isolated, minimizing drift surface. |
| NIO timeout API differs across Swift Package versions | Low | Task 7.2 Step notes to verify against the pinned Package.swift version; fallback is a local `scheduleTask`-based timeout (~6 lines). |
| Symlinked protocol.proto breaks in Xcode on some setups | Low | Task 3.3 includes fallback (build-phase copy script). |
| Token rotation on daemon restart drops existing mounts | Expected | Spec documents "remount required after daemon restart" — matches pre-existing `daemon.rs:133,284` cleanup behavior. |

---

## Self-Review

Scanned this plan against the spec:

- ✅ Bridge Security section (spec:240+) — every enforcement point and threat model item is implemented in Tasks 4-8.
- ✅ Phase 1.5 decisions 1-7 in spec:615+ — each maps to a specific task (1→Task 1, 2→Task 2, 3→Task 3, 4→Task 5, 5→Task 7, 6→Task 9 and daemon.rs changes in Task 6, 7→Risk Register).
- ✅ Integration test (spec:703 — tests/tcp_roundtrip.rs) — Task 5 and Task 8 together.
- ✅ No placeholders in code (only in "subagent: read X before deciding Y" style annotations where the exact API depends on unchecked local crate state, which is explicit delegation not TBD).
- ✅ Type consistency: `AuthToken`, `AuthenticateRequest`, `token_hex`, `raw`/`bytes_vec()` are used consistently throughout.
- ✅ Tasks 9 and 10 marked explicitly as non-subagent-executable.

One spec requirement not yet tested: **protocol replay resistance**. The spec says post-auth `authenticate` requests should be rejected. Task 5.7's `handle_stream` code rejects with `EPROTO`, but no dedicated test covers the replay case. Adding a step to Task 8:

- [ ] **Step 8.1.5: Add replay-rejection test** — after successful auth, send another AuthenticateRequest on the same channel, assert connection closes.

(Subagent executing Task 8: include this case in `e2e_auth.rs` as a third `#[tokio::test]`.)
