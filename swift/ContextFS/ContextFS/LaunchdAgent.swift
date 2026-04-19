import Foundation

enum LaunchdAgent {
    static let label = "ai.ctxfs.daemon"

    static var plistURL: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/LaunchAgents/\(label).plist")
    }

    static var logDirURL: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Logs/ContextFS")
    }

    static var isInstalled: Bool {
        FileManager.default.fileExists(atPath: plistURL.path)
    }

    /// Install the launchd agent pointing at the given ctxfs binary.
    /// If ctxfsBinaryURL is nil, resolves via Bundle.main.url(forAuxiliaryExecutable:).
    static func install(ctxfsBinaryURL: URL? = nil) throws {
        let ctxfsPath: URL
        if let url = ctxfsBinaryURL {
            ctxfsPath = url
        } else if let bundled = Bundle.main.url(forAuxiliaryExecutable: "ctxfs") {
            ctxfsPath = bundled
        } else {
            throw LaunchdError.ctxfsBinaryNotFound
        }

        // Ensure log directory exists
        try FileManager.default.createDirectory(at: logDirURL, withIntermediateDirectories: true)

        let plist: [String: Any] = [
            "Label": label,
            "ProgramArguments": [ctxfsPath.path, "daemon", "start"],
            "RunAtLoad": true,
            "KeepAlive": true,
            "StandardOutPath": logDirURL.appendingPathComponent("daemon.log").path,
            "StandardErrorPath": logDirURL.appendingPathComponent("daemon.err").path,
        ]

        let data = try PropertyListSerialization.data(
            fromPropertyList: plist, format: .xml, options: 0)

        try FileManager.default.createDirectory(
            at: plistURL.deletingLastPathComponent(), withIntermediateDirectories: true)
        try data.write(to: plistURL, options: .atomic)

        // Load via launchctl
        try runLaunchctl(args: ["load", plistURL.path])
    }

    static func uninstall() throws {
        guard isInstalled else { return }
        // Best-effort unload
        _ = try? runLaunchctl(args: ["unload", plistURL.path])
        try FileManager.default.removeItem(at: plistURL)
    }

    private static func runLaunchctl(args: [String]) throws {
        let task = Process()
        task.executableURL = URL(fileURLWithPath: "/bin/launchctl")
        task.arguments = args
        try task.run()
        task.waitUntilExit()
        if task.terminationStatus != 0 {
            throw LaunchdError.launchctlFailed(exitCode: task.terminationStatus)
        }
    }

    enum LaunchdError: LocalizedError {
        case ctxfsBinaryNotFound
        case launchctlFailed(exitCode: Int32)

        var errorDescription: String? {
            switch self {
            case .ctxfsBinaryNotFound:
                return "Could not locate the ctxfs binary inside ContextFS.app. Try reinstalling the app."
            case .launchctlFailed(let code):
                return "launchctl failed with exit code \(code)"
            }
        }
    }
}
