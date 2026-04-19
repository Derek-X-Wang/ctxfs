import Foundation

// MARK: - MountStatus

/// Mirrors the Rust `MountStatus` enum serialized by serde's default representation:
/// unit variants → plain strings ("Ready", "Mounting", "Unmounting")
/// tuple variant → {"Error": "message"}
public enum MountStatus: Hashable {
    case mounting
    case ready
    case error(String)
    case unmounting
    case unknown(String)
}

extension MountStatus: Codable {
    public init(from decoder: Decoder) throws {
        // Try decoding as a plain string first (unit variants).
        if let single = try? decoder.singleValueContainer(),
           let str = try? single.decode(String.self) {
            switch str {
            case "Mounting":   self = .mounting
            case "Ready":      self = .ready
            case "Unmounting": self = .unmounting
            default:           self = .unknown(str)
            }
            return
        }
        // Otherwise try decoding as {"Error": "message"}.
        let container = try decoder.container(keyedBy: DynamicCodingKey.self)
        if let errorKey = container.allKeys.first(where: { $0.stringValue == "Error" }) {
            let msg = try container.decode(String.self, forKey: errorKey)
            self = .error(msg)
            return
        }
        self = .unknown("unknown")
    }

    public func encode(to encoder: Encoder) throws {
        switch self {
        case .mounting:
            var c = encoder.singleValueContainer()
            try c.encode("Mounting")
        case .ready:
            var c = encoder.singleValueContainer()
            try c.encode("Ready")
        case .unmounting:
            var c = encoder.singleValueContainer()
            try c.encode("Unmounting")
        case .error(let msg):
            var c = encoder.container(keyedBy: DynamicCodingKey.self)
            try c.encode(msg, forKey: DynamicCodingKey(stringValue: "Error")!)
        case .unknown(let s):
            var c = encoder.singleValueContainer()
            try c.encode(s)
        }
    }

    private struct DynamicCodingKey: CodingKey {
        var stringValue: String
        var intValue: Int? { nil }
        init?(stringValue: String) { self.stringValue = stringValue }
        init?(intValue: Int) { return nil }
    }
}

// MARK: - MountInfo

public struct MountInfo: Codable, Identifiable, Hashable {
    public let id: String
    public let source: String
    public let mountPoint: String
    public let commitSha: String?
    public let status: MountStatus
    public let backend: String
    public let mountedAt: String
    /// NFS loopback port (NFS mounts only).
    public let nfsPort: UInt16?
    /// Filesystem path to volume (FSKit mounts only).
    public let volumePath: String?
    /// Convenience symlink paths tracked for this mount.
    public let symlinkPaths: [String]

    public init(
        id: String,
        source: String,
        mountPoint: String,
        commitSha: String?,
        status: MountStatus,
        backend: String,
        mountedAt: String,
        nfsPort: UInt16? = nil,
        volumePath: String? = nil,
        symlinkPaths: [String] = []
    ) {
        self.id = id
        self.source = source
        self.mountPoint = mountPoint
        self.commitSha = commitSha
        self.status = status
        self.backend = backend
        self.mountedAt = mountedAt
        self.nfsPort = nfsPort
        self.volumePath = volumePath
        self.symlinkPaths = symlinkPaths
    }

    enum CodingKeys: String, CodingKey {
        case id, source
        case mountPoint = "mount_point"
        case commitSha = "commit_sha"
        case status, backend
        case mountedAt = "mounted_at"
        case nfsPort = "nfs_port"
        case volumePath = "volume_path"
        case symlinkPaths = "symlink_paths"
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        source = try container.decode(String.self, forKey: .source)
        mountPoint = try container.decode(String.self, forKey: .mountPoint)
        commitSha = try container.decodeIfPresent(String.self, forKey: .commitSha)
        status = try container.decode(MountStatus.self, forKey: .status)
        backend = try container.decode(String.self, forKey: .backend)
        mountedAt = try container.decode(String.self, forKey: .mountedAt)
        nfsPort = try container.decodeIfPresent(UInt16.self, forKey: .nfsPort)
        volumePath = try container.decodeIfPresent(String.self, forKey: .volumePath)
        symlinkPaths = try container.decodeIfPresent([String].self, forKey: .symlinkPaths) ?? []
    }
}

// MARK: - CacheBreakdown

public struct CacheBreakdown: Codable {
    public let blobBytes: UInt64
    public let blobCount: UInt64
    public let treeBytes: UInt64
    public let treeCount: UInt64
    public let maxBytes: UInt64

    public init(blobBytes: UInt64, blobCount: UInt64, treeBytes: UInt64, treeCount: UInt64, maxBytes: UInt64) {
        self.blobBytes = blobBytes
        self.blobCount = blobCount
        self.treeBytes = treeBytes
        self.treeCount = treeCount
        self.maxBytes = maxBytes
    }

    enum CodingKeys: String, CodingKey {
        case blobBytes = "blob_bytes"
        case blobCount = "blob_count"
        case treeBytes = "tree_bytes"
        case treeCount = "tree_count"
        case maxBytes = "max_bytes"
    }
}

// MARK: - ExtensionStatus

public struct ExtensionStatus: Codable {
    public let bundleId: String
    public let registered: Bool
    public let enabled: Bool
    public let version: String?
    public let platformSupported: Bool

    public init(bundleId: String, registered: Bool, enabled: Bool, version: String?, platformSupported: Bool) {
        self.bundleId = bundleId
        self.registered = registered
        self.enabled = enabled
        self.version = version
        self.platformSupported = platformSupported
    }

    enum CodingKeys: String, CodingKey {
        case bundleId = "bundle_id"
        case registered, enabled, version
        case platformSupported = "platform_supported"
    }
}

// MARK: - TokenValidation

public struct TokenValidation: Codable {
    public let valid: Bool
    public let user: String?
    public let remaining: UInt64?
    public let resetAt: String?

    public init(valid: Bool, user: String?, remaining: UInt64?, resetAt: String?) {
        self.valid = valid
        self.user = user
        self.remaining = remaining
        self.resetAt = resetAt
    }

    enum CodingKeys: String, CodingKey {
        case valid, user, remaining
        case resetAt = "reset_at"
    }
}

// MARK: - ConfigSnapshot

public struct ConfigSnapshot: Codable {
    public let path: String
    public let content: String
    public let snapshotHash: String

    public init(path: String, content: String, snapshotHash: String) {
        self.path = path
        self.content = content
        self.snapshotHash = snapshotHash
    }

    enum CodingKeys: String, CodingKey {
        case path, content
        case snapshotHash = "snapshot_hash"
    }
}

// MARK: - JSONValue (for configSetValue)

/// Generic JSON-encodable value type for parameterising `configSetValue`.
public enum JSONValue: Encodable {
    case string(String)
    case bool(Bool)
    case int(Int64)
    case double(Double)

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .string(let s): try container.encode(s)
        case .bool(let b):   try container.encode(b)
        case .int(let i):    try container.encode(i)
        case .double(let d): try container.encode(d)
        }
    }
}
