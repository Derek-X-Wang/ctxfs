import XCTest
@testable import AppCore

final class ModelsTests: XCTestCase {
    func testMountInfoDecode() throws {
        let json = """
        {
            "id": "github_octocat_Hello-World_master",
            "source": "github:octocat/Hello-World@master",
            "mount_point": "./test-mnt",
            "commit_sha": "7fd1a60b",
            "status": "Ready",
            "backend": "fskit",
            "mounted_at": "2026-04-19T00:00:00Z"
        }
        """
        let info = try JSONDecoder().decode(MountInfo.self, from: json.data(using: .utf8)!)
        XCTAssertEqual(info.id, "github_octocat_Hello-World_master")
        XCTAssertEqual(info.mountPoint, "./test-mnt")
        XCTAssertEqual(info.commitSha, "7fd1a60b")
        XCTAssertEqual(info.source, "github:octocat/Hello-World@master")
        XCTAssertEqual(info.mountedAt, "2026-04-19T00:00:00Z")
    }

    func testMountInfoStatusErrorDecode() throws {
        let json = """
        {
            "id": "err_mount",
            "source": "github:a/b@main",
            "mount_point": "/tmp/mnt",
            "commit_sha": "000000",
            "status": {"Error": "FUSE unavailable"},
            "backend": "nfs",
            "mounted_at": "2026-04-19T00:00:00Z"
        }
        """
        let info = try JSONDecoder().decode(MountInfo.self, from: json.data(using: .utf8)!)
        if case .error(let msg) = info.status {
            XCTAssertEqual(msg, "FUSE unavailable")
        } else {
            XCTFail("Expected .error status, got \(info.status)")
        }
    }

    func testMountInfoOptionalFields() throws {
        // status as plain string "Mounting", no commit_sha
        let json = """
        {
            "id": "mount_no_sha",
            "source": "npm:react@19.0.0",
            "mount_point": "/tmp/npm-mnt",
            "status": "Mounting",
            "backend": "nfs",
            "mounted_at": "2026-04-19T00:00:00Z"
        }
        """
        let info = try JSONDecoder().decode(MountInfo.self, from: json.data(using: .utf8)!)
        XCTAssertNil(info.commitSha)
        if case .mounting = info.status { } else {
            XCTFail("Expected .mounting status, got \(info.status)")
        }
    }

    func testCacheBreakdownDecode() throws {
        let json = """
        {"blob_bytes":1024,"blob_count":5,"tree_bytes":512,"tree_count":2,"max_bytes":10000}
        """
        let b = try JSONDecoder().decode(CacheBreakdown.self, from: json.data(using: .utf8)!)
        XCTAssertEqual(b.blobBytes, 1024)
        XCTAssertEqual(b.blobCount, 5)
        XCTAssertEqual(b.treeBytes, 512)
        XCTAssertEqual(b.treeCount, 2)
        XCTAssertEqual(b.maxBytes, 10000)
    }

    func testExtensionStatusDecode() throws {
        let json = """
        {"bundle_id":"ai.ctxfs.fskitbridge.fskitext","registered":true,"enabled":true,"version":null,"platform_supported":true}
        """
        let s = try JSONDecoder().decode(ExtensionStatus.self, from: json.data(using: .utf8)!)
        XCTAssertEqual(s.bundleId, "ai.ctxfs.fskitbridge.fskitext")
        XCTAssertTrue(s.registered)
        XCTAssertTrue(s.enabled)
        XCTAssertNil(s.version)
        XCTAssertTrue(s.platformSupported)
    }

    func testExtensionStatusWithVersion() throws {
        let json = """
        {"bundle_id":"ai.ctxfs.fskitbridge.fskitext","registered":true,"enabled":false,"version":"1.2.3","platform_supported":true}
        """
        let s = try JSONDecoder().decode(ExtensionStatus.self, from: json.data(using: .utf8)!)
        XCTAssertEqual(s.version, "1.2.3")
        XCTAssertFalse(s.enabled)
    }

    func testTokenValidationDecode() throws {
        let json = """
        {"valid":true,"user":"derekxwang","remaining":4987,"reset_at":"2026-04-19T01:30:00Z"}
        """
        let v = try JSONDecoder().decode(TokenValidation.self, from: json.data(using: .utf8)!)
        XCTAssertTrue(v.valid)
        XCTAssertEqual(v.user, "derekxwang")
        XCTAssertEqual(v.remaining, 4987)
        XCTAssertEqual(v.resetAt, "2026-04-19T01:30:00Z")
    }

    func testTokenValidationInvalidDecode() throws {
        let json = """
        {"valid":false,"user":null,"remaining":null,"reset_at":null}
        """
        let v = try JSONDecoder().decode(TokenValidation.self, from: json.data(using: .utf8)!)
        XCTAssertFalse(v.valid)
        XCTAssertNil(v.user)
        XCTAssertNil(v.remaining)
        XCTAssertNil(v.resetAt)
    }

    func testConfigSnapshotDecode() throws {
        let json = """
        {"path":"/Users/test/.ctxfs/config.toml","content":"github_token = \\"abc\\"\\n","snapshot_hash":"sha256abc123"}
        """
        let c = try JSONDecoder().decode(ConfigSnapshot.self, from: json.data(using: .utf8)!)
        XCTAssertEqual(c.path, "/Users/test/.ctxfs/config.toml")
        XCTAssertTrue(c.content.contains("github_token"))
        XCTAssertEqual(c.snapshotHash, "sha256abc123")
    }
}
