import XCTest
@testable import AppCore

final class DaemonStateTests: XCTestCase {

    // MARK: - iconState transitions

    @MainActor
    func testIconStateIsErrorWhenDaemonDown() {
        let state = DaemonState(client: MockHelperClient())
        state.daemonRunning = false
        XCTAssertEqual(state.iconState, .error)
    }

    @MainActor
    func testIconStateIsSetupNeededWhenExtensionDisabled() {
        let state = DaemonState(client: MockHelperClient())
        state.daemonRunning = true
        state.extensionStatus = ExtensionStatus(
            bundleId: "ai.ctxfs.fskitbridge.fskitext",
            registered: true, enabled: false, version: nil, platformSupported: true
        )
        XCTAssertEqual(state.iconState, .setupNeeded)
    }

    @MainActor
    func testIconStateIsActiveWithMounts() {
        let state = DaemonState(client: MockHelperClient())
        state.daemonRunning = true
        state.extensionStatus = ExtensionStatus(
            bundleId: "x", registered: true, enabled: true, version: nil, platformSupported: true
        )
        state.mounts = [.stub]
        XCTAssertEqual(state.iconState, .active)
    }

    @MainActor
    func testIconStateIsIdleWhenHealthyNoMounts() {
        let state = DaemonState(client: MockHelperClient())
        state.daemonRunning = true
        state.extensionStatus = ExtensionStatus(
            bundleId: "x", registered: true, enabled: true, version: nil, platformSupported: true
        )
        state.mounts = []
        XCTAssertEqual(state.iconState, .idle)
    }

    @MainActor
    func testIconStateIgnoresExtensionOnNonMacOS() {
        // platform_supported=false means we're not on macOS; no FSKit possible;
        // extension_enabled=false isn't a problem in that case.
        let state = DaemonState(client: MockHelperClient())
        state.daemonRunning = true
        state.extensionStatus = ExtensionStatus(
            bundleId: "x", registered: false, enabled: false, version: nil, platformSupported: false
        )
        state.mounts = []
        XCTAssertEqual(state.iconState, .idle)
    }

    // MARK: - Poll loop (integration)

    @MainActor
    func testPollLoopUpdatesMountsFromClient() async throws {
        let mock = MockHelperClient()
        mock.mountsResponse = [.stub]
        mock.pingResponse = "pong"
        mock.extensionStatusResponse = ExtensionStatus(
            bundleId: "x", registered: true, enabled: true, version: nil, platformSupported: true
        )
        mock.cacheBreakdownResponse = CacheBreakdown(
            blobBytes: 1024, blobCount: 5, treeBytes: 512, treeCount: 2, maxBytes: 10_000
        )

        let state = DaemonState(client: mock, pollInterval: 0.05)  // faster for test
        state.start()

        // Wait for at least one poll cycle
        try await Task.sleep(nanoseconds: 200_000_000)

        XCTAssertTrue(state.daemonRunning)
        XCTAssertEqual(state.mounts.count, 1)
        XCTAssertEqual(state.mounts.first?.id, MountInfo.stub.id)
        XCTAssertEqual(state.cacheBreakdown?.blobCount, 5)

        state.stop()
    }

    @MainActor
    func testPollLoopSetsDaemonRunningFalseWhenPingFails() async throws {
        let mock = MockHelperClient()
        mock.shouldFailPing = true

        let state = DaemonState(client: mock, pollInterval: 0.05)
        state.start()

        try await Task.sleep(nanoseconds: 200_000_000)

        XCTAssertFalse(state.daemonRunning)
        XCTAssertNotNil(state.lastError)

        state.stop()
    }
}

// MARK: - MountInfo stub helper
extension MountInfo {
    static let stub = MountInfo(
        id: "test-mount",
        source: "github:owner/repo@main",
        mountPoint: "/tmp/test-mount",
        commitSha: "abc123",
        status: .ready,
        backend: "fskit",
        mountedAt: "2026-04-19T00:00:00Z"
    )
}

// MARK: - Mock HelperClient
final class MockHelperClient: HelperClientProtocol, @unchecked Sendable {
    var pingResponse: String = "pong"
    var shouldFailPing: Bool = false
    var mountsResponse: [MountInfo] = []
    var extensionStatusResponse: ExtensionStatus = ExtensionStatus(
        bundleId: "x", registered: false, enabled: false, version: nil, platformSupported: true
    )
    var cacheBreakdownResponse: CacheBreakdown?

    func ping() async throws -> String {
        if shouldFailPing { throw HelperClientError.helperCrashed(code: -1) }
        return pingResponse
    }
    func list() async throws -> [MountInfo] { mountsResponse }
    func unmount(target: String) async throws { }
    func cacheBreakdown() async throws -> CacheBreakdown {
        cacheBreakdownResponse ?? CacheBreakdown(blobBytes: 0, blobCount: 0, treeBytes: 0, treeCount: 0, maxBytes: 0)
    }
    func setCacheLimits(maxBytes: UInt64) async throws -> CacheBreakdown { try await cacheBreakdown() }
    func pruneBlobs(targetBytes: UInt64) async throws -> UInt64 { 0 }
    func extensionStatus() async throws -> ExtensionStatus { extensionStatusResponse }
    func testGitHubToken(token: String) async throws -> TokenValidation {
        TokenValidation(valid: true, user: nil, remaining: nil, resetAt: nil)
    }
    func configRead() async throws -> ConfigSnapshot {
        ConfigSnapshot(path: "/tmp/config.toml", content: "", snapshotHash: "")
    }
    func configSet(content: String, snapshotHash: String) async throws { }
    func configSetValue(key: String, value: JSONValue) async throws { }
}
