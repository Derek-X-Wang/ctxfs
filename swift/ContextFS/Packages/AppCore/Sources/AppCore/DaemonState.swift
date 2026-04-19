import Foundation
import Observation

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
        // Only flag setupNeeded when FSKit is actually possible on this platform.
        if let ext = extensionStatus, ext.platformSupported, !ext.enabled {
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

        if let m = await mountsResult   { mounts           = m }
        if let e = await extResult      { extensionStatus  = e }
        if let c = await cacheResult    { cacheBreakdown   = c }
    }
}
