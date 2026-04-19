import Foundation

// MARK: - Protocol

/// Abstraction over the concrete `HelperClient` that allows tests to inject a
/// mock without spawning a real subprocess.
public protocol HelperClientProtocol: Sendable {
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
    func configSetValue(key: String, value: JSONValue) async throws
}

// MARK: - Error types

public enum HelperClientError: Error, Equatable {
    case helperNotFound
    case helperCrashed(code: Int32)
    case invalidResponse
    case rpcError(String)
}

// MARK: - HelperClient actor

/// Manages a long-lived `ctxfs-app-helper` subprocess and exposes typed async
/// methods for every helper JSON-RPC method.
///
/// The subprocess is spawned lazily on the first request.  Concurrent requests
/// are matched to responses by the `id` field and resolved via
/// `CheckedContinuation`.  If the process crashes all pending continuations
/// are failed and the process is respawned on the next call.
///
/// Stdout is read via `FileHandle.readabilityHandler` (non-blocking) bridged to
/// a `AsyncStream` so that no cooperative thread is ever blocked.
public actor HelperClient {
    private let helperPath: URL
    private var process: Process?
    private var stdinHandle: FileHandle?
    private var nextID: UInt64 = 1
    private var pending: [UInt64: CheckedContinuation<Data, Error>] = [:]
    private var stdoutReaderTask: Task<Void, Never>?
    // Stream continuation for the readability-handler → async bridge
    private var streamContinuation: AsyncStream<Data>.Continuation?
    // Shared encoder/decoder — reused across requests to avoid per-call allocation.
    private let encoder = JSONEncoder()
    private let decoder = JSONDecoder()

    // MARK: Init

    public init(helperPath: URL? = nil) throws {
        let resolved = helperPath ?? Self.resolveDefaultPath()
        guard FileManager.default.fileExists(atPath: resolved.path) else {
            throw HelperClientError.helperNotFound
        }
        self.helperPath = resolved
    }

    // MARK: Path resolution

    private static func resolveDefaultPath() -> URL {
        // 1. Bundled alongside the app binary (production)
        if let bundleHelper = Bundle.main.url(forAuxiliaryExecutable: "ctxfs-app-helper") {
            return bundleHelper
        }
        // 2. Env var override (dev / CI)
        if let override = ProcessInfo.processInfo.environment["CTXFS_APP_HELPER_PATH"] {
            return URL(fileURLWithPath: override)
        }
        // 3. Fallback
        return URL(fileURLWithPath: "/usr/local/bin/ctxfs-app-helper")
    }

    // MARK: Process lifecycle

    /// Ensure the helper subprocess is running; spawn if not.
    private func ensureRunning() throws {
        if let p = process, p.isRunning { return }

        // Clean up any previous state
        stdoutReaderTask?.cancel()
        stdoutReaderTask = nil
        streamContinuation?.finish()
        streamContinuation = nil
        stdinHandle = nil
        process = nil

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
        self.stdinHandle = stdinPipe.fileHandleForWriting

        // Build a non-blocking AsyncStream backed by readabilityHandler.
        let stdoutHandle = stdoutPipe.fileHandleForReading
        let (stream, streamCont) = AsyncStream<Data>.makeStream()
        self.streamContinuation = streamCont

        // readabilityHandler is called on a background thread whenever data
        // is available; we yield it into the stream.
        // Capture continuation by value (it's a struct) — safe to retain in closure.
        let capturedCont = streamCont
        stdoutHandle.readabilityHandler = { handle in
            let chunk = handle.availableData
            if chunk.isEmpty {
                // EOF — process exited
                capturedCont.finish()
                handle.readabilityHandler = nil
            } else {
                capturedCont.yield(chunk)
            }
        }

        stdoutReaderTask = Task { [weak self] in
            await self?.consumeStream(stream)
        }

        // Drain stderr in the background (for diagnostics panel in future tasks)
        let stderrHandle = stderrPipe.fileHandleForReading
        Task.detached {
            _ = stderrHandle.readDataToEndOfFile()
        }
    }

    // MARK: Stdout consumer

    /// Consumes the AsyncStream of raw chunks, splits on newlines, and dispatches
    /// complete JSON lines to pending continuations.
    private func consumeStream(_ stream: AsyncStream<Data>) async {
        var buffer = Data()

        for await chunk in stream {
            buffer.append(chunk)

            while let newlineIdx = buffer.firstIndex(of: UInt8(ascii: "\n")) {
                let lineData = Data(buffer[buffer.startIndex..<newlineIdx])
                buffer.removeSubrange(buffer.startIndex...newlineIdx)
                if !lineData.isEmpty {
                    dispatchLine(lineData)
                }
            }
        }

        // Stream finished — process exited
        failAllPending(error: HelperClientError.helperCrashed(code: -1))
        process = nil
        stdinHandle = nil
    }

    private func dispatchLine(_ data: Data) {
        struct IDOnly: Decodable { let id: UInt64 }
        guard let idOnly = try? decoder.decode(IDOnly.self, from: data) else { return }
        if let continuation = pending.removeValue(forKey: idOnly.id) {
            continuation.resume(returning: data)
        }
    }

    private func failAllPending(error: Error) {
        let snapshot = pending
        pending.removeAll()
        for (_, cont) in snapshot {
            cont.resume(throwing: error)
        }
    }

    // MARK: Core send

    private struct EmptyParams: Encodable {}

    private struct RequestEnvelope<P: Encodable>: Encodable {
        let id: UInt64
        let method: String
        let params: P?
    }

    private struct ResponseEnvelope<R: Decodable>: Decodable {
        let id: UInt64
        let result: R?
        let error: String?
    }

    /// Send a JSON-RPC request and decode the typed response.
    private func send<P: Encodable, R: Decodable>(
        method: String,
        params: P,
        as _: R.Type
    ) async throws -> R {
        try ensureRunning()

        let id = nextID
        nextID += 1

        let envelope = RequestEnvelope(id: id, method: method, params: params)
        var lineData = try encoder.encode(envelope)
        lineData.append(UInt8(ascii: "\n"))

        guard let stdinHandle = stdinHandle else {
            throw HelperClientError.helperCrashed(code: -1)
        }

        let rawResponse: Data = try await withCheckedThrowingContinuation { cont in
            pending[id] = cont
            do {
                try stdinHandle.write(contentsOf: lineData)
            } catch {
                pending.removeValue(forKey: id)
                cont.resume(throwing: error)
            }
        }

        let env = try decoder.decode(ResponseEnvelope<R>.self, from: rawResponse)
        if let errMsg = env.error {
            throw HelperClientError.rpcError(errMsg)
        }
        guard let result = env.result else {
            throw HelperClientError.invalidResponse
        }
        return result
    }

    /// Convenience overload for methods with no params.
    private func send<R: Decodable>(method: String, as type: R.Type) async throws -> R {
        try await send(method: method, params: EmptyParams(), as: type)
    }

    // MARK: - Typed public API

    /// Ping the helper; returns "pong".
    public func ping() async throws -> String {
        try await send(method: "ping", as: String.self)
    }

    /// List all active mounts.
    public func list() async throws -> [MountInfo] {
        try await send(method: "list", as: [MountInfo].self)
    }

    /// Unmount the volume at the given path or by mount ID.
    public func unmount(target: String) async throws {
        struct Params: Encodable { let target: String }
        struct OkResponse: Decodable { let ok: Bool }
        let resp: OkResponse = try await send(method: "unmount", params: Params(target: target), as: OkResponse.self)
        guard resp.ok else { throw HelperClientError.rpcError("unmount returned ok=false") }
    }

    /// Returns structured blob/tree cache usage and the configured max.
    public func cacheBreakdown() async throws -> CacheBreakdown {
        try await send(method: "cache_breakdown", as: CacheBreakdown.self)
    }

    /// Updates the blob cache max size at runtime; returns fresh breakdown.
    public func setCacheLimits(maxBytes: UInt64) async throws -> CacheBreakdown {
        struct Params: Encodable {
            let maxBytes: UInt64
            enum CodingKeys: String, CodingKey { case maxBytes = "max_bytes" }
        }
        return try await send(method: "set_cache_limits", params: Params(maxBytes: maxBytes), as: CacheBreakdown.self)
    }

    /// Prune blob cache until usage fits within `targetBytes`; returns bytes freed.
    public func pruneBlobs(targetBytes: UInt64) async throws -> UInt64 {
        struct Params: Encodable {
            let targetBytes: UInt64
            enum CodingKeys: String, CodingKey { case targetBytes = "target_bytes" }
        }
        struct PruneResult: Decodable {
            let bytesFreed: UInt64
            enum CodingKeys: String, CodingKey { case bytesFreed = "bytes_freed" }
        }
        let r: PruneResult = try await send(method: "prune_blobs", params: Params(targetBytes: targetBytes), as: PruneResult.self)
        return r.bytesFreed
    }

    /// Query the FSKit extension registration + enabled state via pluginkit.
    public func extensionStatus() async throws -> ExtensionStatus {
        try await send(method: "extension_status", as: ExtensionStatus.self)
    }

    /// Validate a GitHub personal access token; returns rate-limit info.
    public func testGitHubToken(token: String) async throws -> TokenValidation {
        struct Params: Encodable { let token: String }
        return try await send(method: "test_github_token", params: Params(token: token), as: TokenValidation.self)
    }

    /// Read the current config file and its snapshot hash for write-back.
    public func configRead() async throws -> ConfigSnapshot {
        try await send(method: "config_read", as: ConfigSnapshot.self)
    }

    /// Write a new full config content, guarded by snapshot hash (collision detection).
    public func configSet(content: String, snapshotHash: String) async throws {
        struct Params: Encodable {
            let content: String
            let snapshotHash: String
            enum CodingKeys: String, CodingKey {
                case content
                case snapshotHash = "snapshot_hash"
            }
        }
        struct OkResponse: Decodable { let ok: Bool }
        let _: OkResponse = try await send(
            method: "config_set",
            params: Params(content: content, snapshotHash: snapshotHash),
            as: OkResponse.self
        )
    }

    /// Update a single config key in-place using toml_edit (preserves comments).
    public func configSetValue(key: String, value: JSONValue) async throws {
        struct Params: Encodable {
            let key: String
            let value: JSONValue
        }
        struct OkResponse: Decodable { let ok: Bool }
        let _: OkResponse = try await send(
            method: "config_set_value",
            params: Params(key: key, value: value),
            as: OkResponse.self
        )
    }

    // MARK: Shutdown

    /// Terminate the subprocess cleanly and cancel the reader task.
    public func shutdown() {
        stdoutReaderTask?.cancel()
        stdoutReaderTask = nil
        streamContinuation?.finish()
        streamContinuation = nil
        stdinHandle = nil
        process?.terminate()
        process = nil
        failAllPending(error: HelperClientError.helperCrashed(code: -1))
    }
}

// MARK: - HelperClientProtocol conformance

// `HelperClient` is an actor; all its public methods are already `async throws`.
// The protocol signatures match exactly, so conformance is automatic — we just
// declare it here in an extension to keep it explicit.
extension HelperClient: HelperClientProtocol {}
