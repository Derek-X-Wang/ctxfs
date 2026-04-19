import Foundation
import Observation
import SwiftUI

/// Single source of truth for the app's view of the running daemon.
///
/// `DaemonState` is `@MainActor` so that all mutations happen on the main thread
/// and SwiftUI `@Observable` change notifications fire on the correct queue.
///
/// Inject a `HelperClientProtocol` conformer to replace the real subprocess with
/// a mock in unit tests.
@MainActor
@Observable
public final class DaemonState {

    // MARK: - Published state

    public var daemonRunning: Bool = false
    public var mounts: [MountInfo] = []
    public var extensionStatus: ExtensionStatus?
    public var cacheBreakdown: CacheBreakdown?
    public var lastError: String?

    // MARK: - Icon state

    public enum IconState: Equatable {
        /// Daemon up, no mounts, no setup issues.
        case idle
        /// Daemon up, at least one active mount.
        case active
        /// Extension disabled (FSKit required but not yet enabled by user).
        case setupNeeded
        /// Daemon down or unreachable.
        case error
        /// A mount/unmount is in progress.
        case busy
    }

    /// Computed from the current state; drives the menu bar status dot color.
    public var iconState: IconState {
        guard daemonRunning else { return .error }
        // Only flag setupNeeded when the user is actually trying to use FSKit
        // (i.e. a mount is configured to use the fskit backend) but the extension
        // isn't enabled. NFS mounts don't care about the extension status.
        if let ext = extensionStatus, ext.platformSupported, !ext.enabled,
           mounts.contains(where: { $0.backend == "fskit" }) {
            return .setupNeeded
        }
        if !mounts.isEmpty { return .active }
        return .idle
    }

    // MARK: - Private

    private let client: any HelperClientProtocol
    private let pollInterval: TimeInterval
    private var pollTask: Task<Void, Never>?

    // MARK: - Init

    public init(client: any HelperClientProtocol, pollInterval: TimeInterval = 2.0) {
        self.client = client
        self.pollInterval = pollInterval
    }

    // MARK: - Lifecycle

    /// Start the background polling loop.  Calling `start()` a second time is a
    /// no-op if the task is still running.
    public func start() {
        guard pollTask == nil else { return }
        pollTask = Task { [weak self] in
            guard let self else { return }
            while !Task.isCancelled {
                await self.pollOnce()
                try? await Task.sleep(nanoseconds: UInt64(self.pollInterval * 1_000_000_000))
            }
        }
    }

    /// Stop the polling loop and cancel the background task.
    public func stop() {
        pollTask?.cancel()
        pollTask = nil
    }

    // MARK: - Unmount actions

    /// Unmount a single mount point and immediately refresh state.
    public func unmount(_ target: String) async {
        do {
            try await client.unmount(target: target)
            await pollOnce()
        } catch {
            lastError = "unmount failed: \(error)"
        }
    }

    /// Unmount all active mounts and immediately refresh state.
    public func unmountAll() async {
        let targets = mounts.map(\.mountPoint)
        for t in targets {
            try? await client.unmount(target: t)
        }
        await pollOnce()
    }

    // MARK: - Config pass-throughs

    /// Read the current config file and its snapshot hash.
    public func configRead() async throws -> ConfigSnapshot {
        try await client.configRead()
    }

    /// Update a single config key in-place (atomic + comment-preserving on the Rust side).
    public func configSetValue(key: String, value: JSONValue) async throws {
        try await client.configSetValue(key: key, value: value)
    }

    /// Validate a GitHub personal access token; returns rate-limit info.
    public func testGitHubToken(token: String) async throws -> TokenValidation {
        try await client.testGitHubToken(token: token)
    }

    /// Update the blob cache max size at runtime; returns fresh breakdown and updates `cacheBreakdown`.
    @discardableResult
    public func setCacheLimits(maxBytes: UInt64) async throws -> CacheBreakdown {
        let result = try await client.setCacheLimits(maxBytes: maxBytes)
        cacheBreakdown = result
        return result
    }

    /// Prune blob cache to `targetBytes`; refreshes `cacheBreakdown` and returns bytes freed.
    @discardableResult
    public func pruneBlobs(targetBytes: UInt64) async throws -> UInt64 {
        let freed = try await client.pruneBlobs(targetBytes: targetBytes)
        if let b = try? await client.cacheBreakdown() {
            cacheBreakdown = b
        }
        return freed
    }

    // MARK: - Private poll

    private func pollOnce() async {
        // Ping the daemon first; if it's unreachable, skip the other requests and
        // clear stale state so the UI shows the error dot immediately.
        do {
            _ = try await client.ping()
            daemonRunning = true
            lastError = nil
        } catch {
            daemonRunning = false
            mounts = []
            cacheBreakdown = nil
            lastError = "\(error)"
            // Still try to refresh extension_status — pluginkit query doesn't
            // require the daemon to be running.
            if let ext = try? await client.extensionStatus() {
                extensionStatus = ext
            }
            return
        }

        // Fetch the remaining endpoints in parallel for minimal latency.
        async let mountsResult   = try? client.list()
        async let extResult      = try? client.extensionStatus()
        async let cacheResult    = try? client.cacheBreakdown()

        if let m = await mountsResult,   m != mounts           { mounts           = m }
        if let e = await extResult,      e != extensionStatus  { extensionStatus  = e }
        if let c = await cacheResult,    c != cacheBreakdown   { cacheBreakdown   = c }
    }
}

// MARK: - IconState color helper

public extension DaemonState.IconState {
    /// Maps each icon state to its status-dot color (nil = no dot).
    ///
    /// Both `StatusIcon` (menu bar) and `StatusDot` (MenuContent header)
    /// use this single source of truth.
    var statusDotColor: Color? {
        switch self {
        case .idle:        return nil
        case .active:      return .green
        case .setupNeeded: return .orange
        case .error:       return .red
        case .busy:        return .blue
        }
    }
}
