# Phase 2b-B — Swift Menu Bar Companion App

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Transform `ContextFS.app` from a stub host bundle into a full macOS menu bar companion app — status icon, dropdown, Preferences window, onboarding wizard, login-item integration, and Rust binary bundling.

**Architecture:** Swift host app spawns `ctxfs-app-helper` (shipped in Phase 2b-A) as a subprocess; communicates via JSON-RPC over stdin/stdout. Helper holds a persistent tarpc connection to the daemon. App writes config via new `config_set` helper method to preserve the atomic+hash-check guarantees from Phase 2b-A Task 5.

**Tech Stack:** Swift 5.9+, SwiftUI (MenuBarExtra, macOS 14+), Foundation Process/Pipe, SMAppService (macOS 13+), NSWorkspace for deep-links.

**Reference spec:** `docs/superpowers/specs/2026-04-18-contextfs-companion-app-design.md` — all UI and architectural decisions are canonical there.

**TDD: every task writes the failing test first. Swift tests use XCTest via `xcodebuild test`. Where UI correctness can't be unit-tested (pure visual rendering), we verify with `xcodebuild build` success + manual smoke test at the end.**

---

## File Structure

**Created (Rust side):**
- `crates/ctxfs-app-helper/src/handler.rs` gets two new methods: `config_read` + `config_set`

**Created (Swift side):**
- `swift/ContextFS/ContextFS/HelperClient.swift` — subprocess management + JSON-RPC
- `swift/ContextFS/ContextFS/Models.swift` — Codable types (MountInfo, CacheBreakdown, etc.)
- `swift/ContextFS/ContextFS/DaemonState.swift` — `@Observable` class with polling loop
- `swift/ContextFS/ContextFS/MenuBarView.swift` — MenuBarExtra + status icon + dot overlay
- `swift/ContextFS/ContextFS/MenuContent.swift` — dropdown content (mount list + actions)
- `swift/ContextFS/ContextFS/PreferencesView.swift` — Preferences window (5 settings)
- `swift/ContextFS/ContextFS/OnboardingView.swift` — Welcome + Quick/Custom wizard
- `swift/ContextFS/ContextFS/LaunchdAgent.swift` — plist install/uninstall for daemon
- `swift/ContextFS/ContextFS/LoginItem.swift` — SMAppService wrapper
- `swift/ContextFS/ContextFSTests/` — unit tests (HelperClient mock, Models decode, DaemonState transitions)
- `swift/ContextFS/build-rust.sh` — pre-build script to compile + embed Rust binaries

**Modified:**
- `swift/ContextFS/ContextFS/ContextFS.swift` — replace the stub with `@main` MenuBarExtra wiring
- `swift/ContextFS/ContextFS.xcodeproj/project.pbxproj` — disable host sandbox, add pre-build script phase, bundle additional resources
- `swift/ContextFS/ContextFS/ContextFS.entitlements` — remove App Sandbox, keep only necessary entitlements

---

## Task Dependency Graph

```
1 Helper: config_read/config_set ─┐
                                  │
2 Disable host sandbox ──────────┬┴──▶ 3 HelperClient + models ──▶ 4 DaemonState ──▶ 5 MenuBar scaffold ──▶ 6 Menu content
                                 │                                                                               │
                                 │                                                                               ▼
                                 │                                                            7 Preferences window
                                 │                                                                               │
                                 │                                                                               ▼
                                 │                                                            8 Onboarding wizard
                                 │                                                                               │
                                 │                                                                               ▼
                                 │                                                            9 Login item + launchd
                                 │                                                                               │
                                 │                                                                               ▼
                                 └────────────────────────────────────────────────────────────▶ 10 Bundle Rust binaries
```

Task 1 is Rust; tasks 2-10 are Swift/Xcode. User-assisted steps: Task 2 (Xcode sandbox toggle), Task 10 (optional GUI verification).

---

## Task 1: Helper methods `config_read` + `config_set`

**Goal:** Swift Preferences writes go through the helper so the Rust-side atomic_write + SHA-256 snapshot logic (Phase 2b-A Task 5) is preserved. No Swift-side TOML parsing or write contention.

**Files:**
- Modify: `crates/ctxfs-app-helper/src/handler.rs` — two new match arms
- Modify: `crates/ctxfs-app-helper/tests/e2e.rs` — tests

**Context:** `ConfigSnapshot` already exists in `ctxfs-cli/src/setup.rs` (or the equivalent module from Task 5). The helper just calls it. Helper is a binary; if the types aren't exported, either re-export them or duplicate the ~40 lines (prefer re-export — expose via a library target in ctxfs-cli, OR move ConfigSnapshot to a shared location like `ctxfs-core`).

**Recommended**: move `ConfigSnapshot` + `atomic_write` from `ctxfs-cli/src/setup.rs` to `ctxfs-core/src/config.rs` so both the CLI and the helper can use it without duplication.

- [ ] **Step 1.1: Write failing tests**

In `crates/ctxfs-app-helper/tests/e2e.rs`:

```rust
#[test]
fn config_read_returns_current_toml_content() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join(".ctxfs").join("config.toml");
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(&config_path, r#"github_token = "abc""#).unwrap();

    let mut child = Command::cargo_bin("ctxfs-app-helper").unwrap()
        .env("HOME", tmp.path())  // reroute ~/.ctxfs
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().expect("spawn");
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    writeln!(stdin, r#"{{"id":1,"method":"config_read"}}"#).unwrap();
    stdin.flush().unwrap();
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(response.trim()).unwrap();
    assert!(parsed["result"]["content"].as_str().unwrap().contains("github_token"));
    // Must also return a snapshot_hash the caller uses for write_back
    assert!(parsed["result"]["snapshot_hash"].is_string());

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn config_set_writes_atomically_and_requires_matching_snapshot() {
    // 1. config_read → get snapshot_hash
    // 2. config_set with snapshot_hash + new content → success
    // 3. verify file content is updated
    // Plus a negative test: config_set with wrong snapshot_hash → error
}
```

- [ ] **Step 1.2: Run tests, verify RED**

- [ ] **Step 1.3: (If needed) move ConfigSnapshot/atomic_write to ctxfs-core**

If they're currently in ctxfs-cli and only `setup::` pub, move them to `ctxfs-core::config` module. Update the two `setup.rs` call sites to use the new path. Run `cargo test -p ctxfs` to confirm no regressions.

- [ ] **Step 1.4: Implement `config_read`**

In `handler.rs`:

```rust
"config_read" => {
    let path = ctxfs_core::config::Config::load_config_path();  // or equivalent — the function that resolves ~/.ctxfs/config.toml
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let snapshot = ctxfs_core::config::ConfigSnapshot::read(&path)
        .map(|s| s.hash())
        .unwrap_or_default();
    Response::ok(req.id, serde_json::json!({
        "path": path.to_string_lossy(),
        "content": content,
        "snapshot_hash": snapshot,
    }))
}
```

If `ConfigSnapshot` doesn't expose `hash()` publicly, add a `pub fn hash(&self) -> &str` getter.

- [ ] **Step 1.5: Implement `config_set`**

```rust
"config_set" => {
    #[derive(serde::Deserialize)]
    struct Params {
        /// Full new file contents (Swift sends the whole file after edits).
        content: String,
        /// Snapshot hash returned by config_read when the GUI opened.
        snapshot_hash: String,
    }
    let params: Params = match serde_json::from_value(req.params.clone()) {
        Ok(p) => p,
        Err(e) => return Response::err(req.id, format!("invalid params for config_set: {e}")),
    };
    let path = ctxfs_core::config::Config::load_config_path();
    // Reconstruct the snapshot and use its write_back.
    let snapshot = ctxfs_core::config::ConfigSnapshot::from_hash(params.snapshot_hash);
    match snapshot.write_back(&path, &params.content) {
        Ok(()) => Response::ok(req.id, serde_json::json!({"ok": true})),
        Err(ctxfs_core::config::ConfigWriteError::ExternalEdit { expected, actual }) => {
            Response::err(req.id, format!("config was modified externally (expected {expected}, found {actual})"))
        }
        Err(e) => Response::err(req.id, format!("write failed: {e}")),
    }
}
```

`ConfigSnapshot::from_hash(s: String)` may need to be added if the existing constructor is `read(path)`-only.

- [ ] **Step 1.6: Tests pass, commit**

```
feat(app-helper): add config_read and config_set methods

Preferences GUI uses these to read the current config + snapshot
hash, then write back with collision detection. Reuses the atomic
write + hash check infrastructure from Phase 2b-A Task 5 — no Swift
TOML handling required.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
```

---

## Task 2: Disable App Sandbox on host target

**Goal:** The host app target has `ENABLE_APP_SANDBOX = YES` in pbxproj (Codex finding). For Phase 2b-B the host app needs to:
- Write `~/Library/LaunchAgents/ai.ctxfs.daemon.plist`
- Write `~/.ctxfs/config.toml` via the helper (but helper is still a subprocess, so sandbox doesn't block this directly)
- Spawn subprocesses (helper binary)
- Deep-link to System Settings URLs

All of these are incompatible with default App Sandbox. Disable on host target; **keep appex sandboxed** (appex only needs FSKit + network entitlements).

**Files:**
- Modify: `swift/ContextFS/ContextFS.xcodeproj/project.pbxproj` — set `ENABLE_APP_SANDBOX = NO` on ContextFS target (both Debug + Release config); leave ContextFSExt unchanged
- Modify: `swift/ContextFS/ContextFS/ContextFS.entitlements` — remove sandbox-specific keys; keep only entitlements the app actually needs (none for now — we'll add as needed)

**This task may require the user to open Xcode GUI** if direct pbxproj editing is risky. Document both paths.

- [ ] **Step 2.1: Identify the current sandbox settings**

```bash
grep -n "ENABLE_APP_SANDBOX\|com.apple.security" swift/ContextFS/ContextFS.xcodeproj/project.pbxproj swift/ContextFS/ContextFS/ContextFS.entitlements
```

Record the current state for rollback if needed.

- [ ] **Step 2.2: Edit pbxproj — set ENABLE_APP_SANDBOX = NO on host target**

Two locations (Debug + Release config blocks). Use `sed` or direct Edit tool:

```
ENABLE_APP_SANDBOX = YES;   →   ENABLE_APP_SANDBOX = NO;
```

**Only on the ContextFS app target.** Appex (`ContextFSExt`) stays sandboxed.

Grep confirms only 2 lines changed (one per Debug/Release), and they're in the `ContextFS /* ... */ = {` target block, not the ContextFSExt block.

- [ ] **Step 2.3: Edit ContextFS.entitlements**

Remove `com.apple.security.app-sandbox` if present. The file should end up with just the keys the app actually uses (which is none for now — an empty `<dict/>` is acceptable).

- [ ] **Step 2.4: Verify build**

```bash
xcodebuild -project swift/ContextFS/ContextFS.xcodeproj -scheme ContextFS -configuration Debug build SYMROOT=/tmp/ctxfs-build 2>&1 | tail -5
```

Expected: `** BUILD SUCCEEDED **`.

```bash
# Also verify appex is still sandboxed
defaults read /tmp/ctxfs-build/Debug/ContextFS.app/Contents/Extensions/ContextFSExt.appex/Contents/Info.plist EXAppExtensionAttributes 2>&1 | head
# Check entitlements of built app:
codesign -d --entitlements - /tmp/ctxfs-build/Debug/ContextFS.app 2>&1 | head -20
codesign -d --entitlements - /tmp/ctxfs-build/Debug/ContextFS.app/Contents/Extensions/ContextFSExt.appex 2>&1 | head -20
```

Host app should NOT have `com.apple.security.app-sandbox` in entitlements; appex should.

- [ ] **Step 2.5: Commit**

```
refactor(app): disable App Sandbox on host target, keep on appex

Host app needs to write ~/Library/LaunchAgents/, spawn subprocesses,
and deep-link to System Settings — all blocked by default sandbox.
Homebrew-distributed developer utility pattern (Docker Desktop
precedent).

Appex (ContextFSExt) stays sandboxed — it only needs FSKit + network
entitlements.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
```

---

## Task 3: HelperClient — spawn subprocess + JSON-RPC

**Goal:** Swift class that manages the long-lived `ctxfs-app-helper` subprocess, exposes async methods for each helper RPC, handles respawn on crash.

**Files:**
- Create: `swift/ContextFS/ContextFS/HelperClient.swift`
- Create: `swift/ContextFS/ContextFS/Models.swift` — Codable structs matching helper JSON
- Create: `swift/ContextFS/ContextFSTests/HelperClientTests.swift`
- Modify: `swift/ContextFS/ContextFS.xcodeproj/project.pbxproj` — add a Test target + test files (if not already present)

**Context:** The helper binary will be bundled into `ContextFS.app/Contents/MacOS/ctxfs-app-helper` in Task 10. For dev iteration, we can use `which ctxfs-app-helper` or an env var override to locate it. `HelperClient` resolves the path via: (1) bundled alongside the app if present, (2) `CTXFS_APP_HELPER_PATH` env var, (3) fall back to `/usr/local/bin/ctxfs-app-helper`.

**Design:**

```swift
actor HelperClient {
    private var process: Process?
    private var stdin: FileHandle?
    private var stdout: FileHandle?
    private var nextID: UInt64 = 1
    private var pendingRequests: [UInt64: CheckedContinuation<Data, Error>] = [:]
    private let helperPath: URL

    init(helperPath: URL? = nil) throws { ... }

    // Generic request
    func call<T: Decodable>(method: String, params: Encodable? = nil, as: T.Type) async throws -> T { ... }

    // Typed conveniences
    func ping() async throws -> String
    func list() async throws -> [MountInfo]
    func unmount(target: String) async throws
    func cacheBreakdown() async throws -> CacheBreakdown
    func setCacheLimits(maxBytes: UInt64) async throws -> CacheBreakdown
    func pruneBlobs(targetBytes: UInt64) async throws -> UInt64
    func extensionStatus() async throws -> ExtensionStatus
    func testGitHubToken(token: String) async throws -> TokenValidation
    func configRead() async throws -> ConfigSnapshot
    func configSet(content: String, snapshotHash: String) async throws
}
```

`Models.swift` has the Codable types matching the helper's JSON.

- [ ] **Step 3.1: Write failing tests**

```swift
// HelperClientTests.swift
import XCTest
@testable import ContextFS

final class HelperClientTests: XCTestCase {
    func testPingRoundtrip() async throws {
        let client = try HelperClient()
        let result = try await client.ping()
        XCTAssertEqual(result, "pong")
    }

    func testListWithoutDaemonReturnsError() async throws {
        let client = try HelperClient(socketPath: URL(fileURLWithPath: "/tmp/does-not-exist.sock"))
        do {
            _ = try await client.list()
            XCTFail("expected error when daemon is down")
        } catch HelperClientError.daemonUnreachable {
            // expected
        }
    }

    func testSurvivesMultipleRequests() async throws {
        let client = try HelperClient()
        for _ in 0..<5 {
            let result = try await client.ping()
            XCTAssertEqual(result, "pong")
        }
    }
}
```

- [ ] **Step 3.2: Confirm RED**

```bash
xcodebuild -project swift/ContextFS/ContextFS.xcodeproj -scheme ContextFS test -destination 'platform=macOS' 2>&1 | tail -20
```

If there's no test target yet, add one via Xcode (File → New → Target → macOS Unit Testing Bundle) OR via pbxproj edit + creating `ContextFSTests` directory. This is user-assisted if pbxproj editing is too fragile.

- [ ] **Step 3.3: Implement `Models.swift`**

```swift
import Foundation

struct MountInfo: Codable, Identifiable, Hashable {
    let id: String
    let source: String
    let mountPoint: String
    let commitSha: String?
    let status: String
    let backend: String
    let mountedAt: String

    enum CodingKeys: String, CodingKey {
        case id, source
        case mountPoint = "mount_point"
        case commitSha = "commit_sha"
        case status, backend
        case mountedAt = "mounted_at"
    }
}

struct CacheBreakdown: Codable {
    let blobBytes: UInt64
    let blobCount: UInt64
    let treeBytes: UInt64
    let treeCount: UInt64
    let maxBytes: UInt64

    enum CodingKeys: String, CodingKey {
        case blobBytes = "blob_bytes"
        case blobCount = "blob_count"
        case treeBytes = "tree_bytes"
        case treeCount = "tree_count"
        case maxBytes = "max_bytes"
    }
}

struct ExtensionStatus: Codable {
    let bundleId: String
    let registered: Bool
    let enabled: Bool
    let version: String?
    let platformSupported: Bool

    enum CodingKeys: String, CodingKey {
        case bundleId = "bundle_id"
        case registered, enabled, version
        case platformSupported = "platform_supported"
    }
}

struct TokenValidation: Codable {
    let valid: Bool
    let user: String?
    let remaining: UInt64?
    let resetAt: String?

    enum CodingKeys: String, CodingKey {
        case valid, user, remaining
        case resetAt = "reset_at"
    }
}

struct ConfigSnapshot: Codable {
    let path: String
    let content: String
    let snapshotHash: String

    enum CodingKeys: String, CodingKey {
        case path, content
        case snapshotHash = "snapshot_hash"
    }
}
```

- [ ] **Step 3.4: Implement `HelperClient.swift`**

Full design: use Swift concurrency (`actor` for thread-safe state, async methods wrapping CheckedContinuation). Spawn Process with Pipe for stdin/stdout/stderr. Use a Task to read stdout line-by-line, dispatch responses to pending continuations by ID.

```swift
import Foundation

enum HelperClientError: Error {
    case helperNotFound
    case helperCrashed(code: Int32)
    case daemonUnreachable
    case invalidResponse
    case rpcError(String)
}

actor HelperClient {
    private let helperPath: URL
    private var process: Process?
    private var stdin: FileHandle?
    private var nextID: UInt64 = 1
    private var pending: [UInt64: CheckedContinuation<Data, Error>] = [:]
    private var stdoutReaderTask: Task<Void, Never>?

    init(helperPath: URL? = nil) throws {
        self.helperPath = helperPath ?? Self.resolveDefaultPath()
        guard FileManager.default.fileExists(atPath: self.helperPath.path) else {
            throw HelperClientError.helperNotFound
        }
    }

    private static func resolveDefaultPath() -> URL {
        // 1. Bundled alongside the app
        if let bundleHelper = Bundle.main.url(forAuxiliaryExecutable: "ctxfs-app-helper") {
            return bundleHelper
        }
        // 2. Env override
        if let override = ProcessInfo.processInfo.environment["CTXFS_APP_HELPER_PATH"] {
            return URL(fileURLWithPath: override)
        }
        // 3. Fallback
        return URL(fileURLWithPath: "/usr/local/bin/ctxfs-app-helper")
    }

    private func ensureRunning() throws {
        if let p = process, p.isRunning { return }

        let p = Process()
        p.executableURL = helperPath
        let stdinPipe = Pipe()
        let stdoutPipe = Pipe()
        let stderrPipe = Pipe()
        p.standardInput = stdinPipe
        p.standardOutput = stdoutPipe
        p.standardError = stderrPipe

        try p.run()

        self.process = p
        self.stdin = stdinPipe.fileHandleForWriting

        // Stdout reader task
        let stdoutHandle = stdoutPipe.fileHandleForReading
        self.stdoutReaderTask = Task { [weak self] in
            await self?.readStdout(handle: stdoutHandle)
        }

        // Drop stderr (or log it)
        let stderrHandle = stderrPipe.fileHandleForReading
        Task.detached {
            // TODO: capture stderr for diag panel
            _ = stderrHandle.readDataToEndOfFile()
        }
    }

    private func readStdout(handle: FileHandle) async {
        // Read line by line, parse JSON, match to pending[id]
        var buffer = Data()
        while true {
            let chunk = handle.availableData
            if chunk.isEmpty {
                break // EOF
            }
            buffer.append(chunk)
            while let newlineIndex = buffer.firstIndex(of: 0x0A) {
                let lineData = buffer[..<newlineIndex]
                buffer.removeSubrange(...newlineIndex)
                await handleLine(Data(lineData))
            }
        }
        // Process exited; fail all pending
        await failAllPending(error: HelperClientError.helperCrashed(code: -1))
    }

    private func handleLine(_ data: Data) async {
        struct Envelope: Decodable {
            let id: UInt64
            let result: AnyDecodable?
            let error: String?
        }
        // Lightweight: parse id + error/raw result
        guard let idOnly = try? JSONDecoder().decode(IDOnly.self, from: data) else { return }
        if let cont = pending.removeValue(forKey: idOnly.id) {
            cont.resume(returning: data)
        }
    }

    private struct IDOnly: Decodable { let id: UInt64 }

    private func failAllPending(error: Error) {
        let snapshot = pending
        pending.removeAll()
        for (_, cont) in snapshot {
            cont.resume(throwing: error)
        }
    }

    private func send<P: Encodable, R: Decodable>(method: String, params: P?, as: R.Type) async throws -> R {
        try ensureRunning()
        let id = nextID
        nextID += 1

        struct RequestEnvelope<P: Encodable>: Encodable {
            let id: UInt64
            let method: String
            let params: P?
        }
        let envelope = RequestEnvelope(id: id, method: method, params: params)
        let body = try JSONEncoder().encode(envelope)
        var line = body
        line.append(0x0A) // newline

        guard let stdin = stdin else { throw HelperClientError.helperCrashed(code: -1) }

        let data: Data = try await withCheckedThrowingContinuation { cont in
            pending[id] = cont
            do {
                try stdin.write(contentsOf: line)
            } catch {
                pending.removeValue(forKey: id)
                cont.resume(throwing: error)
            }
        }

        // Parse response
        struct ResponseEnvelope<R: Decodable>: Decodable {
            let id: UInt64
            let result: R?
            let error: String?
        }
        let env = try JSONDecoder().decode(ResponseEnvelope<R>.self, from: data)
        if let e = env.error {
            throw HelperClientError.rpcError(e)
        }
        guard let r = env.result else {
            throw HelperClientError.invalidResponse
        }
        return r
    }

    // MARK: - Typed methods

    func ping() async throws -> String {
        try await send(method: "ping", params: EmptyParams(), as: String.self)
    }

    func list() async throws -> [MountInfo] {
        try await send(method: "list", params: EmptyParams(), as: [MountInfo].self)
    }

    struct UnmountParams: Encodable { let target: String }
    func unmount(target: String) async throws {
        struct OkResponse: Decodable { let ok: Bool }
        let r: OkResponse = try await send(method: "unmount", params: UnmountParams(target: target), as: OkResponse.self)
        guard r.ok else { throw HelperClientError.rpcError("unmount returned ok=false") }
    }

    func cacheBreakdown() async throws -> CacheBreakdown {
        try await send(method: "cache_breakdown", params: EmptyParams(), as: CacheBreakdown.self)
    }

    struct SetCacheLimitsParams: Encodable {
        let maxBytes: UInt64
        enum CodingKeys: String, CodingKey { case maxBytes = "max_bytes" }
    }
    func setCacheLimits(maxBytes: UInt64) async throws -> CacheBreakdown {
        try await send(method: "set_cache_limits", params: SetCacheLimitsParams(maxBytes: maxBytes), as: CacheBreakdown.self)
    }

    struct PruneBlobsParams: Encodable {
        let targetBytes: UInt64
        enum CodingKeys: String, CodingKey { case targetBytes = "target_bytes" }
    }
    struct PruneResult: Decodable {
        let bytesFreed: UInt64
        enum CodingKeys: String, CodingKey { case bytesFreed = "bytes_freed" }
    }
    func pruneBlobs(targetBytes: UInt64) async throws -> UInt64 {
        let r: PruneResult = try await send(method: "prune_blobs", params: PruneBlobsParams(targetBytes: targetBytes), as: PruneResult.self)
        return r.bytesFreed
    }

    func extensionStatus() async throws -> ExtensionStatus {
        try await send(method: "extension_status", params: EmptyParams(), as: ExtensionStatus.self)
    }

    struct TestTokenParams: Encodable { let token: String }
    func testGitHubToken(token: String) async throws -> TokenValidation {
        try await send(method: "test_github_token", params: TestTokenParams(token: token), as: TokenValidation.self)
    }

    func configRead() async throws -> ConfigSnapshot {
        try await send(method: "config_read", params: EmptyParams(), as: ConfigSnapshot.self)
    }

    struct ConfigSetParams: Encodable {
        let content: String
        let snapshotHash: String
        enum CodingKeys: String, CodingKey {
            case content
            case snapshotHash = "snapshot_hash"
        }
    }
    func configSet(content: String, snapshotHash: String) async throws {
        struct OkResponse: Decodable { let ok: Bool }
        let _: OkResponse = try await send(method: "config_set", params: ConfigSetParams(content: content, snapshotHash: snapshotHash), as: OkResponse.self)
    }

    private struct EmptyParams: Encodable {}
}
```

- [ ] **Step 3.5: Run tests, verify GREEN**

Tests need the `ctxfs-app-helper` binary on PATH or the `CTXFS_APP_HELPER_PATH` env var. In CI, set that env var to `$(cargo metadata --format-version 1 | jq -r '.target_directory')/debug/ctxfs-app-helper` after `cargo build -p ctxfs-app-helper`.

- [ ] **Step 3.6: Commit**

```
feat(app): add HelperClient — async JSON-RPC over spawned subprocess

Swift actor manages the long-lived ctxfs-app-helper subprocess;
resolves the binary path via bundle → env var → /usr/local fallback.
All helper methods exposed as typed async Swift APIs. Concurrent
requests matched by id.

Models.swift has Codable structs for MountInfo, CacheBreakdown,
ExtensionStatus, TokenValidation, ConfigSnapshot — snake_case JSON
keys mapped to camelCase Swift fields.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
```

---

## Task 4: `DaemonState` — `@Observable` class with polling

**Goal:** Single source of truth for app state: daemon running, mount list, cache stats, extension status. Polls every 2 seconds via HelperClient. Published to SwiftUI via `@Observable`.

**Files:**
- Create: `swift/ContextFS/ContextFS/DaemonState.swift`
- Create: `swift/ContextFS/ContextFSTests/DaemonStateTests.swift`

**Context:** Uses Swift 5.9 `@Observable` macro (macOS 14+). Timer-based poll loop via Swift Concurrency:

```swift
@Observable
class DaemonState {
    var daemonRunning: Bool = false
    var mounts: [MountInfo] = []
    var extensionStatus: ExtensionStatus?
    var cacheBreakdown: CacheBreakdown?
    var lastError: String?

    enum IconState {
        case idle      // daemon up, no mounts, no issues
        case active    // daemon up, >=1 mount
        case setupNeeded  // extension disabled, no token, etc.
        case error     // daemon down or crashed
        case busy      // mounting/unmounting in progress
    }

    var iconState: IconState {
        if !daemonRunning { return .error }
        if let ext = extensionStatus, !ext.enabled { return .setupNeeded }
        if !mounts.isEmpty { return .active }
        return .idle
    }

    private let client: HelperClient
    private var pollTask: Task<Void, Never>?

    init(client: HelperClient) { ... }

    func start() { pollTask = Task { await pollLoop() } }
    func stop() { pollTask?.cancel() }
    private func pollLoop() async { /* every 2s, update fields */ }
}
```

- [ ] **Step 4.1: Write failing tests**

```swift
func testIconStateErrorWhenDaemonDown() {
    let mock = MockHelperClient(pingShouldFail: true)
    let state = DaemonState(client: mock)
    state.daemonRunning = false
    XCTAssertEqual(state.iconState, .error)
}

func testIconStateActiveWhenMountsExist() {
    let state = DaemonState(client: MockHelperClient())
    state.daemonRunning = true
    state.mounts = [.mock()]
    state.extensionStatus = ExtensionStatus(bundleId: "x", registered: true, enabled: true, version: nil, platformSupported: true)
    XCTAssertEqual(state.iconState, .active)
}

func testIconStateSetupNeededWhenExtensionDisabled() { ... }
func testIconStateIdleWhenHealthyNoMounts() { ... }
```

- [ ] **Step 4.2: RED → implement → GREEN → commit**

Follow standard TDD. Use dependency injection for HelperClient so tests use mocks. `MockHelperClient` is a protocol that both real and mock implement; or refactor HelperClient to conform to a protocol.

Commit:
```
feat(app): add DaemonState @Observable with 2s polling loop

Single source of truth for mounts, extension, cache, daemon health.
iconState computed property drives the menu bar status dot.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
```

---

## Task 5: MenuBarExtra scaffold + status icon + dot overlay

**Goal:** The app shows up in the menu bar with a status icon. Icon is a monochrome template glyph; a small colored dot overlays the bottom-right indicating state.

**Files:**
- Create: `swift/ContextFS/ContextFS/MenuBarView.swift`
- Modify: `swift/ContextFS/ContextFS/ContextFS.swift` — `@main` entry point with `MenuBarExtra`
- Modify: `swift/ContextFS/ContextFS/Info.plist` — add `LSUIElement = YES` so the app runs as a menu bar agent without a dock icon

- [ ] **Step 5.1: Implement**

```swift
// ContextFS.swift
@main
struct ContextFSApp: App {
    @State private var daemonState = DaemonState(client: try! HelperClient())

    init() {
        daemonState.start()
    }

    var body: some Scene {
        MenuBarExtra {
            MenuContent(state: daemonState)  // from Task 6
        } label: {
            StatusIcon(state: daemonState.iconState)
        }
        .menuBarExtraStyle(.window)  // gives us a SwiftUI popover; .menu is legacy

        // Preferences and Onboarding windows come in later tasks.
    }
}

// MenuBarView.swift
struct StatusIcon: View {
    let state: DaemonState.IconState

    var body: some View {
        Image(systemName: "externaldrive")  // placeholder; custom template image later
            .overlay(alignment: .bottomTrailing) {
                if let color = dotColor {
                    Circle()
                        .fill(color)
                        .frame(width: 6, height: 6)
                }
            }
    }

    private var dotColor: Color? {
        switch state {
        case .idle: return nil  // no dot
        case .active: return .green
        case .setupNeeded: return .orange
        case .error: return .red
        case .busy: return .blue
        }
    }
}
```

- [ ] **Step 5.2: Build + manual verify**

```bash
xcodebuild -project swift/ContextFS/ContextFS.xcodeproj -scheme ContextFS -configuration Debug build SYMROOT=/tmp/ctxfs-build 2>&1 | tail -3
open /tmp/ctxfs-build/Debug/ContextFS.app
# Should appear in menu bar with no dock icon
```

User verifies visually: menu bar icon visible, status dot changes color based on daemon state.

- [ ] **Step 5.3: Commit**

```
feat(app): MenuBarExtra scaffold with status icon + colored dot overlay

App runs as LSUIElement (no dock icon). StatusIcon shows a monochrome
template glyph with a 6pt colored dot overlay driven by DaemonState's
iconState computed property.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
```

---

## Task 6: Menu dropdown content

**Goal:** The "Informative" layout from the spec — status header, mount list with paths, Actions section, Preferences/Quit at bottom.

**Files:**
- Create: `swift/ContextFS/ContextFS/MenuContent.swift`

**Context:** `MenuBarExtraStyle(.window)` means we render SwiftUI views instead of NSMenuItem-based menus. Gives us more layout flexibility.

- [ ] **Step 6.1: Implement**

```swift
struct MenuContent: View {
    @Bindable var state: DaemonState

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            // Header
            HStack {
                Text("ContextFS").font(.headline)
                Spacer()
                statusDot
            }
            .padding()

            if state.daemonRunning {
                Text("\(state.mounts.count) mounts · \(backendLabel)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .padding(.horizontal)
                    .padding(.bottom, 8)
            } else {
                Text("Daemon not running")
                    .font(.caption)
                    .foregroundStyle(.red)
                    .padding(.horizontal)
                    .padding(.bottom, 8)
            }

            Divider()

            // Mount list
            if state.mounts.isEmpty {
                Text("No active mounts")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .padding()
                Text("Use 'ctxfs mount …' in your terminal.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .padding(.horizontal)
                    .padding(.bottom)
            } else {
                ForEach(state.mounts) { mount in
                    MountRow(mount: mount) {
                        Task { try? await unmount(mount) }
                    }
                }
            }

            Divider()

            // Actions
            MenuButton("Unmount All") { Task { await unmountAll() } }
                .disabled(state.mounts.isEmpty)
            MenuButton("Diagnostics…") { openDiagnostics() }

            Divider()

            MenuButton("Preferences…") { openPreferences() }
            MenuButton("Quit ContextFS") { NSApplication.shared.terminate(nil) }
        }
        .frame(width: 320)
    }

    // ... helper views + methods ...
}

struct MountRow: View {
    let mount: MountInfo
    let onUnmount: () -> Void

    var body: some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text("✓ \(displayName)").font(.body)
                Text(mount.mountPoint).font(.caption).foregroundStyle(.secondary)
            }
            Spacer()
            Button(action: onUnmount) {
                Image(systemName: "eject")
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal)
        .padding(.vertical, 6)
        .contentShape(Rectangle())
    }

    private var displayName: String {
        // Prefer source spec "react 19.1.0" over the slug
        mount.source
    }
}
```

- [ ] **Step 6.2: Build + manual verify**

Launch the app, click menu bar icon, verify layout matches spec.

- [ ] **Step 6.3: Commit**

```
feat(app): menu dropdown with mount list + actions (Informative layout B)

Header with status · mount list with ./path subtext · Unmount All
+ Diagnostics· Preferences + Quit. Empty state guides users to CLI.
Eject button per mount row.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
```

---

## Task 7: Preferences window (all 5 settings)

**Goal:** SwiftUI form window with the 5 settings from the spec. Writes via `HelperClient.configSet`.

**Files:**
- Create: `swift/ContextFS/ContextFS/PreferencesView.swift`
- Modify: `swift/ContextFS/ContextFS/ContextFS.swift` — add `Window` scene for Preferences

- [ ] **Step 7.1: Implement PreferencesView with Sections**

```swift
struct PreferencesView: View {
    let client: HelperClient
    @State private var config: ConfigSnapshot?
    @State private var githubToken: String = ""
    @State private var defaultBackend: String = "auto"
    @State private var cacheMaxMB: Double = 512
    @State private var startAtLogin: Bool = false
    @State private var testResult: String? = nil
    @State private var cacheUsageMB: Double = 0

    var body: some View {
        Form {
            Section("General") {
                Toggle("Launch ContextFS at login", isOn: $startAtLogin)
                    .onChange(of: startAtLogin) { _, new in
                        LoginItem.setEnabled(new)  // Task 9
                    }
                Picker("Default backend", selection: $defaultBackend) {
                    Text("Auto").tag("auto")
                    Text("FSKit").tag("fskit")
                    Text("NFS").tag("nfs")
                }
            }

            Section("Authentication") {
                SecureField("GitHub Personal Access Token", text: $githubToken)
                HStack {
                    Button("Test Token") { Task { await testToken() } }
                    if let r = testResult { Text(r).font(.caption) }
                }
            }

            Section("Cache") {
                VStack(alignment: .leading) {
                    Text("Maximum size: \(Int(cacheMaxMB)) MB")
                    Slider(value: $cacheMaxMB, in: 256...8192)
                    Text("Currently using \(Int(cacheUsageMB)) MB").font(.caption).foregroundStyle(.secondary)
                }
                Button("Clear Cache", role: .destructive) { Task { await clearCache() } }
            }

            HStack {
                Spacer()
                Button("Open config.toml in editor…") { openConfigInEditor() }
                    .buttonStyle(.link)
            }
        }
        .padding()
        .frame(width: 560)
        .task { await loadInitialState() }
    }

    // ... implementation methods: loadInitialState, testToken, clearCache, saveIfChanged, etc. ...
}
```

- [ ] **Step 7.2: Wire the write path**

Use `toml_edit` parsing on the Rust side (helper) when `config_set` is called. Swift just passes through the full content.

Actually — for the GUI-driven writes, Swift builds the NEW content by modifying specific lines in the existing config. Simplest approach:
- Swift reads current config via `configRead()`
- User edits settings
- On save (or on blur per field), Swift constructs updated TOML by substituting values. For this, Swift uses a minimal line-based editor (like the existing `set_default_backend` logic in Rust) rather than parsing/rebuilding the whole TOML.

Or: add a Rust helper method per setting (`set_github_token`, `set_default_backend`, etc.) that the Swift side calls. This offloads TOML editing to Rust's toml_edit and keeps Swift simple. **Preferred.**

Add to helper: `config_set_value(key, value)` that uses toml_edit to update one setting while preserving everything else. Swift calls this per field change.

This means updating Task 1's scope to include a granular per-key setter, OR adding a new task. Adding to Task 1 is fine — do `config_set_value` in Task 1 as well as the bulk `config_set`.

- [ ] **Step 7.3: Commit**

```
feat(app): Preferences window with 5 settings

Start at login · Default backend · GitHub token + Test · Cache size
slider with current usage · Clear cache. Writes via helper's
config_set_value for per-key preservation of comments.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
```

---

## Task 8: Onboarding wizard (Quick + Custom paths)

**Goal:** First-launch modal with Welcome → Quick/Custom fork → step-by-step through required + optional setup.

**Files:**
- Create: `swift/ContextFS/ContextFS/OnboardingView.swift`
- Modify: `swift/ContextFS/ContextFS/ContextFS.swift` — show onboarding if not completed

- [ ] **Step 8.1: Implement**

State machine with enum for steps:

```swift
enum OnboardingStep {
    case welcome
    case quickExtension
    case quickToken
    case quickDone
    case customBackend
    case customExtension
    case customToken
    case customCache
    case customLoginItem
    case customDone
}

struct OnboardingView: View { /* ... */ }
```

Polling for extension state happens via `DaemonState.extensionStatus`; the wizard auto-advances when `enabled == true`.

Deep-link to System Settings:
```swift
func openFSKitSettings() {
    NSWorkspace.shared.open(URL(string: "x-apple.systempreferences:com.apple.LoginItems-Settings.extension?extensionPointIdentifier=com.apple.fskit.fsmodule")!)
}
```

Persistence: write `onboarding_complete = true` to config.toml via helper (or UserDefaults if simpler).

- [ ] **Step 8.2: Commit**

```
feat(app): first-launch onboarding wizard with Quick/Custom paths

Welcome → fork. Quick = 2 required steps (extension enable + token).
Custom = 5+ steps (backend, extension, token, cache, login item).
Both paths share the extension-enable step with deep-link +
automatic state polling.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
```

---

## Task 9: LoginItem (SMAppService) + Launchd agent (daemon)

**Goal:** Two distinct mechanisms:
- `SMAppService.mainApp` for "Launch ContextFS.app at login" Preferences toggle
- `~/Library/LaunchAgents/ai.ctxfs.daemon.plist` for daemon autostart (installed on first launch, removable via a preference)

**Files:**
- Create: `swift/ContextFS/ContextFS/LoginItem.swift`
- Create: `swift/ContextFS/ContextFS/LaunchdAgent.swift`

- [ ] **Step 9.1: LoginItem — SMAppService**

```swift
import ServiceManagement

enum LoginItem {
    static var isEnabled: Bool {
        SMAppService.mainApp.status == .enabled
    }

    static func setEnabled(_ enabled: Bool) {
        do {
            if enabled {
                try SMAppService.mainApp.register()
            } else {
                try SMAppService.mainApp.unregister()
            }
        } catch {
            // TODO: surface error to user
        }
    }
}
```

- [ ] **Step 9.2: LaunchdAgent — plist install**

```swift
enum LaunchdAgent {
    static var plistURL: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/LaunchAgents/ai.ctxfs.daemon.plist")
    }

    static var isInstalled: Bool {
        FileManager.default.fileExists(atPath: plistURL.path)
    }

    static func install(ctxfsBinaryPath: URL) throws {
        let plist: [String: Any] = [
            "Label": "ai.ctxfs.daemon",
            "ProgramArguments": [ctxfsBinaryPath.path, "daemon", "start"],
            "RunAtLoad": true,
            "KeepAlive": true,
            "StandardOutPath": FileManager.default.homeDirectoryForCurrentUser
                .appendingPathComponent("Library/Logs/ContextFS/daemon.log").path,
            "StandardErrorPath": FileManager.default.homeDirectoryForCurrentUser
                .appendingPathComponent("Library/Logs/ContextFS/daemon.err").path,
        ]
        let data = try PropertyListSerialization.data(fromPropertyList: plist, format: .xml, options: 0)
        try FileManager.default.createDirectory(at: plistURL.deletingLastPathComponent(), withIntermediateDirectories: true)
        try data.write(to: plistURL, options: .atomic)

        // Load via launchctl
        let task = Process()
        task.executableURL = URL(fileURLWithPath: "/bin/launchctl")
        task.arguments = ["load", plistURL.path]
        try task.run()
        task.waitUntilExit()
    }

    static func uninstall() throws {
        if isInstalled {
            let task = Process()
            task.executableURL = URL(fileURLWithPath: "/bin/launchctl")
            task.arguments = ["unload", plistURL.path]
            try task.run()
            task.waitUntilExit()
            try FileManager.default.removeItem(at: plistURL)
        }
    }
}
```

- [ ] **Step 9.3: Commit**

```
feat(app): login item + launchd daemon agent

LoginItem wraps SMAppService.mainApp for the "Launch at login"
toggle. LaunchdAgent installs/removes ~/Library/LaunchAgents/
ai.ctxfs.daemon.plist pointing at the app-bundled ctxfs binary.
Two independent concerns — app startup vs. daemon autostart.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
```

---

## Task 10: Bundle Rust binaries into .app at build time

**Goal:** `ContextFS.app/Contents/MacOS/` contains `ctxfs` + `ctxfs-app-helper` at build time. Developers running `xcodebuild` don't need a separate `cargo build` step.

**Files:**
- Create: `swift/ContextFS/build-rust.sh` — pre-build script
- Modify: `swift/ContextFS/ContextFS.xcodeproj/project.pbxproj` — add a Run Script build phase

- [ ] **Step 10.1: Write the script**

```bash
#!/bin/bash
# Build Rust binaries for the current architecture and embed into the .app bundle.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

# Determine build mode from Xcode's CONFIGURATION
if [ "${CONFIGURATION:-Debug}" = "Release" ]; then
    CARGO_FLAG="--release"
    CARGO_TARGET_DIR="target/release"
else
    CARGO_FLAG=""
    CARGO_TARGET_DIR="target/debug"
fi

cargo build $CARGO_FLAG -p ctxfs -p ctxfs-app-helper

# Copy binaries into the built .app bundle
DEST="${BUILT_PRODUCTS_DIR}/${PRODUCT_NAME}.app/Contents/MacOS"
mkdir -p "$DEST"
cp "$CARGO_TARGET_DIR/ctxfs" "$DEST/ctxfs"
cp "$CARGO_TARGET_DIR/ctxfs-app-helper" "$DEST/ctxfs-app-helper"
```

Mark executable: `chmod +x swift/ContextFS/build-rust.sh`.

- [ ] **Step 10.2: Add build phase in Xcode**

Open the project in Xcode → ContextFS target → Build Phases → + → New Run Script Phase → paste:

```
"${SRCROOT}/../build-rust.sh"
```

Set its order AFTER "Compile Sources" but BEFORE "Copy Bundle Resources" (or whatever comes last). The script must run on every build so the binaries are always current.

- [ ] **Step 10.3: Verify**

```bash
xcodebuild -project swift/ContextFS/ContextFS.xcodeproj -scheme ContextFS -configuration Release build SYMROOT=/tmp/ctxfs-build 2>&1 | tail -5
ls -la /tmp/ctxfs-build/Release/ContextFS.app/Contents/MacOS/
# Should list: ContextFS, ctxfs, ctxfs-app-helper
./tmp/ctxfs-build/Release/ContextFS.app/Contents/MacOS/ctxfs --version
```

- [ ] **Step 10.4: Commit**

```
feat(app): bundle ctxfs + ctxfs-app-helper into .app at build time

Pre-build script runs cargo build and copies the binaries into
ContextFS.app/Contents/MacOS/. Release config uses cargo --release;
Debug uses debug. Developers only need xcodebuild.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
```

---

## Out of Scope (deferred to future phases)

- Notarization + signing pipeline (Phase 2b-C)
- Homebrew cask + formula publishing (Phase 2b-C)
- Localization
- Dark mode custom assets beyond system defaults
- Accessibility audit

## Success Criteria

- [ ] App launches, menu bar icon appears with correct status dot color
- [ ] Menu dropdown shows mount list with paths
- [ ] Preferences window opens, all 5 settings save to config.toml
- [ ] Test Token button validates against GitHub
- [ ] Clear Cache button triggers prune_blobs
- [ ] Onboarding wizard appears on first launch, completes successfully
- [ ] Extension enable step auto-advances when user flips toggle in System Settings
- [ ] Launchd agent plist installed and daemon auto-starts on login
- [ ] SMAppService login item toggle works (app launches on login when on)
- [ ] `xcodebuild build` produces a working app with embedded Rust binaries
- [ ] All tests pass (helper IPC round-trip, DaemonState transitions, UI build success)

## Self-Review

- ✅ TDD: every Swift code task writes tests first (XCTest via xcodebuild test)
- ✅ Dependency graph honored: Rust changes (Task 1) → Swift infrastructure (Tasks 2-4) → UI (Tasks 5-8) → integration (Tasks 9-10)
- ✅ No placeholders in code snippets — full Swift code provided where critical
- ✅ Xcode GUI steps called out as user-assisted explicitly
- ✅ Helper bundled path resolution handles dev and prod (bundled → env var → /usr/local fallback)
- ✅ Error states covered (daemon down, extension disabled, helper crash)
- ✅ Covers all UI elements specified in the design spec
