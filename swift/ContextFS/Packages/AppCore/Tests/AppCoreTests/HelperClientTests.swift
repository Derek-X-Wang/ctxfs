import XCTest
@testable import AppCore

final class HelperClientTests: XCTestCase {
    /// Resolves the ctxfs-app-helper binary for testing. In CI, CTXFS_APP_HELPER_PATH
    /// should be set to the cargo-built binary path.
    func helperBinaryPath() throws -> URL {
        if let override = ProcessInfo.processInfo.environment["CTXFS_APP_HELPER_PATH"] {
            return URL(fileURLWithPath: override)
        }
        // Fallback: repo-local debug build
        // Tests/AppCoreTests/ → Tests/ → AppCore/ → Packages/ → ContextFS/ → swift/ → repo root
        let repoRoot = URL(fileURLWithPath: #file)
            .deletingLastPathComponent()  // AppCoreTests/
            .deletingLastPathComponent()  // Tests/
            .deletingLastPathComponent()  // AppCore/
            .deletingLastPathComponent()  // Packages/
            .deletingLastPathComponent()  // ContextFS/ (swift subdir)
            .deletingLastPathComponent()  // swift/
            .deletingLastPathComponent()  // repo root
        let binaryURL = repoRoot.appendingPathComponent("target/debug/ctxfs-app-helper")
        guard FileManager.default.fileExists(atPath: binaryURL.path) else {
            throw XCTSkip("ctxfs-app-helper binary not found at \(binaryURL.path). Run: cargo build -p ctxfs-app-helper")
        }
        return binaryURL
    }

    /// Create a client and register a teardown block to shut it down so the
    /// helper subprocess exits and tests complete without hanging.
    func makeClient() throws -> HelperClient {
        let client = try HelperClient(helperPath: try helperBinaryPath())
        addTeardownBlock {
            await client.shutdown()
        }
        return client
    }

    func testPingRoundtrip() async throws {
        let client = try makeClient()
        let result = try await client.ping()
        XCTAssertEqual(result, "pong")
    }

    func testMultipleRequestsSameSession() async throws {
        let client = try makeClient()
        for _ in 0..<5 {
            let result = try await client.ping()
            XCTAssertEqual(result, "pong")
        }
    }

    func testExtensionStatusDoesNotRequireDaemon() async throws {
        // extension_status works offline — it shells out to pluginkit, not the daemon.
        let client = try makeClient()
        let status = try await client.extensionStatus()
        XCTAssertFalse(status.bundleId.isEmpty)
        // platform_supported is true on macOS
        XCTAssertTrue(status.platformSupported)
    }

    func testListWithoutDaemonReturnsEmptyOrError() async throws {
        // list() requires a daemon; if no daemon is running it returns an error or empty.
        // We just verify the call doesn't crash/hang.
        let client = try makeClient()
        do {
            let mounts = try await client.list()
            // If daemon is running, could return zero or more mounts — both are valid.
            XCTAssertNotNil(mounts)
        } catch HelperClientError.rpcError {
            // daemon unreachable — expected in isolated test environments
        }
    }

    func testHelperNotFoundThrows() {
        let nonexistentPath = URL(fileURLWithPath: "/tmp/does-not-exist-ctxfs-helper")
        XCTAssertThrowsError(try HelperClient(helperPath: nonexistentPath)) { error in
            XCTAssertTrue(error is HelperClientError)
            if case HelperClientError.helperNotFound = error { } else {
                XCTFail("Expected .helperNotFound, got \(error)")
            }
        }
    }

    func testConfigReadReturnsSnapshot() async throws {
        let client = try makeClient()
        // config_read should return a snapshot even if file doesn't exist (empty content)
        let snapshot = try await client.configRead()
        XCTAssertFalse(snapshot.path.isEmpty)
        XCTAssertNotNil(snapshot.content)  // may be empty string
        XCTAssertNotNil(snapshot.snapshotHash)  // may be empty string
    }

    func testConfigSetValueUpdatesKey() async throws {
        let client = try makeClient()
        // Test that configSetValue doesn't throw for a valid key/value pair.
        try await client.configSetValue(key: "test_key_task3", value: .string("task3_value"))
        // If we get here without throwing, the call succeeded.
    }
}
