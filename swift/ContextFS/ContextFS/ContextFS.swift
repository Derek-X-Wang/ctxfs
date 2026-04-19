import SwiftUI
import AppCore

@main
struct ContextFSApp: App {
    @State private var daemonState: DaemonState

    init() {
        // HelperClient resolves the binary path itself (bundle → env var → /usr/local/bin).
        // If the helper isn't present we fall back to NoopHelperClient so the app still
        // launches and shows the error dot rather than crashing.
        let client: any HelperClientProtocol
        do {
            client = try HelperClient()
        } catch {
            client = NoopHelperClient(error: error)
        }

        _daemonState = State(initialValue: DaemonState(client: client))
    }

    @State private var showPreferences: Bool = false

    var body: some Scene {
        MenuBarExtra {
            MenuContent(state: daemonState, showPreferences: $showPreferences)
                .task { daemonState.start() }
        } label: {
            StatusIcon(state: daemonState.iconState)
        }
        .menuBarExtraStyle(.window)

        Window("ContextFS Preferences", id: "preferences") {
            PreferencesView(state: daemonState)
        }
        .windowResizability(.contentSize)
    }
}

// MARK: - Noop fallback client

/// Fallback used when the helper binary can't be resolved at launch.
/// Every method throws the original error so `DaemonState` transitions to `.error`.
final class NoopHelperClient: HelperClientProtocol, @unchecked Sendable {
    let error: Error
    init(error: Error) { self.error = error }
    func ping() async throws -> String { throw error }
    func list() async throws -> [MountInfo] { throw error }
    func unmount(target: String) async throws { throw error }
    func cacheBreakdown() async throws -> CacheBreakdown { throw error }
    func setCacheLimits(maxBytes: UInt64) async throws -> CacheBreakdown { throw error }
    func pruneBlobs(targetBytes: UInt64) async throws -> UInt64 { throw error }
    func extensionStatus() async throws -> ExtensionStatus { throw error }
    func testGitHubToken(token: String) async throws -> TokenValidation { throw error }
    func configRead() async throws -> ConfigSnapshot { throw error }
    func configSet(content: String, snapshotHash: String) async throws { throw error }
    func configSetValue(key: String, value: JSONValue) async throws { throw error }
}
