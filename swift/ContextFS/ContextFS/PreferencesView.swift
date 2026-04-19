import SwiftUI
import AppCore

struct PreferencesView: View {
    @Bindable var state: DaemonState

    // MARK: - Form state (initialised from config.toml on appear)

    @State private var launchAtLogin: Bool = false
    @State private var defaultBackend: BackendChoice = .auto
    @State private var githubToken: String = ""
    @State private var cacheMaxMB: Double = 512
    @State private var tokenTestResult: TokenTestResult = .none
    @State private var isClearingCache: Bool = false
    @State private var initialLoadDone: Bool = false
    @State private var errorMessage: String?

    // MARK: - Nested types

    enum BackendChoice: String, CaseIterable, Identifiable {
        case auto, fskit, nfs
        var id: String { rawValue }
        var displayName: String {
            switch self {
            case .auto:  return "Auto (FSKit on macOS 26+)"
            case .fskit: return "FSKit"
            case .nfs:   return "NFS"
            }
        }
    }

    enum TokenTestResult: Equatable {
        case none
        case testing
        case valid(user: String?, remaining: UInt64?)
        case invalid(String)

        var display: String {
            switch self {
            case .none:    return ""
            case .testing: return "Testing…"
            case .valid(let user, let remaining):
                let parts = [
                    user.map { "user: \($0)" },
                    remaining.map { "\($0) req remaining" },
                ].compactMap { $0 }
                return "✓ Valid (\(parts.joined(separator: ", ")))"
            case .invalid(let reason): return "✗ \(reason)"
            }
        }

        var color: Color {
            switch self {
            case .none, .testing: return .secondary
            case .valid:          return .green
            case .invalid:        return .red
            }
        }
    }

    // MARK: - Body

    var body: some View {
        Form {
            // ------------------------------------------------------------------
            Section("General") {
                Toggle("Launch ContextFS at login", isOn: $launchAtLogin)
                    .onChange(of: launchAtLogin) { _, newValue in
                        // Real wiring arrives in Task 9 via SMAppService.
                        // For now we persist the preference to config.toml.
                        saveValue(key: "launch_at_login", value: .bool(newValue))
                    }

                Picker("Default backend", selection: $defaultBackend) {
                    ForEach(BackendChoice.allCases) { choice in
                        Text(choice.displayName).tag(choice)
                    }
                }
                .onChange(of: defaultBackend) { _, newValue in
                    saveValue(key: "backend", value: .string(newValue.rawValue))
                }
            }

            // ------------------------------------------------------------------
            Section("Authentication") {
                VStack(alignment: .leading, spacing: 8) {
                    SecureField("GitHub Personal Access Token", text: $githubToken)
                        .textContentType(.password)
                        .onSubmit { saveToken() }

                    Text("Needed for private repos and to avoid API rate limits (60 req/hr unauthed → 5000 authed).")
                        .font(.caption)
                        .foregroundStyle(.secondary)

                    HStack {
                        Button("Test Token") { Task { await testToken() } }
                            .disabled(githubToken.isEmpty || tokenTestResult == .testing)
                        Text(tokenTestResult.display)
                            .font(.caption)
                            .foregroundStyle(tokenTestResult.color)
                    }
                }
            }

            // ------------------------------------------------------------------
            Section("Cache") {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Maximum size: \(Int(cacheMaxMB)) MB")
                        .font(.subheadline)

                    Slider(value: $cacheMaxMB, in: 256...8192, step: 64)
                        .onChange(of: cacheMaxMB) { _, newValue in
                            Task { await applyCacheLimit(UInt64(newValue) * 1_024 * 1_024) }
                        }

                    if let breakdown = state.cacheBreakdown {
                        let usedMB = breakdown.blobBytes / 1_024 / 1_024
                        Text("Currently using \(usedMB) MB (\(breakdown.blobCount) blobs, \(breakdown.treeBytes / 1_024 / 1_024) MB in tree cache)")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }

                    Button(role: .destructive) {
                        Task { await clearCache() }
                    } label: {
                        if isClearingCache {
                            ProgressView().controlSize(.small)
                        } else {
                            Text("Clear Cache")
                        }
                    }
                    .disabled(isClearingCache)
                }
            }

            // ------------------------------------------------------------------
            if let errorMessage {
                Text(errorMessage)
                    .font(.caption)
                    .foregroundStyle(.red)
            }

            HStack {
                Spacer()
                Button("Open config.toml in editor…") {
                    openConfigInEditor()
                }
                .buttonStyle(.link)
            }
        }
        .formStyle(.grouped)
        .padding()
        .frame(width: 560, height: 540)
        .task {
            if !initialLoadDone {
                await loadInitialValues()
                initialLoadDone = true
            }
        }
    }

    // MARK: - Load

    private func loadInitialValues() async {
        do {
            let snapshot = try await state.configRead()
            // Line-based TOML parser — no Swift TOML library needed for 5 known keys.
            for line in snapshot.content.split(separator: "\n") {
                let trimmed = line.trimmingCharacters(in: .whitespaces)
                guard !trimmed.hasPrefix("#"), !trimmed.isEmpty else { continue }
                if let (key, val) = parseKV(trimmed) {
                    applyKey(key, val)
                }
            }
            errorMessage = nil
        } catch {
            errorMessage = "Failed to read config: \(error)"
        }
        // Sync the slider to the live cache limit if breakdown is already available.
        if let breakdown = state.cacheBreakdown {
            cacheMaxMB = Double(breakdown.maxBytes / 1_024 / 1_024)
        }
    }

    /// Parse `key = value` — strips surrounding quotes and inline comments.
    private func parseKV(_ line: String) -> (String, String)? {
        guard let eq = line.firstIndex(of: "=") else { return nil }
        let key = line[..<eq].trimmingCharacters(in: .whitespaces)
        var value = line[line.index(after: eq)...].trimmingCharacters(in: .whitespaces)
        // Strip inline comment before doing anything else
        if let hashIdx = value.firstIndex(of: "#") {
            value = String(value[..<hashIdx]).trimmingCharacters(in: .whitespaces)
        }
        // Strip surrounding quotes for string values
        if value.hasPrefix("\"") && value.hasSuffix("\"") {
            value = String(value.dropFirst().dropLast())
        }
        return (key, value)
    }

    private func applyKey(_ key: String, _ value: String) {
        switch key {
        case "launch_at_login":
            launchAtLogin = (value == "true")
        case "backend":
            defaultBackend = BackendChoice(rawValue: value) ?? .auto
        case "github_token":
            githubToken = value
        case "cache_max_bytes":
            if let bytes = UInt64(value) {
                cacheMaxMB = Double(bytes / 1_024 / 1_024)
            }
        default:
            break
        }
    }

    // MARK: - Save helpers

    private func saveValue(key: String, value: JSONValue) {
        Task {
            do {
                try await state.configSetValue(key: key, value: value)
                errorMessage = nil
            } catch {
                errorMessage = "Failed to save \(key): \(error)"
            }
        }
    }

    private func saveToken() {
        saveValue(key: "github_token", value: .string(githubToken))
    }

    // MARK: - Actions

    private func testToken() async {
        guard !githubToken.isEmpty else { return }
        tokenTestResult = .testing
        saveToken()  // Persist before testing — user may have just pasted it
        do {
            let result = try await state.testGitHubToken(token: githubToken)
            if result.valid {
                tokenTestResult = .valid(user: result.user, remaining: result.remaining)
            } else {
                tokenTestResult = .invalid("Invalid token")
            }
        } catch {
            tokenTestResult = .invalid("\(error)")
        }
    }

    private func applyCacheLimit(_ maxBytes: UInt64) async {
        do {
            try await state.setCacheLimits(maxBytes: maxBytes)
            // Also persist to config so the limit survives a daemon restart.
            try await state.configSetValue(key: "cache_max_bytes", value: .int(Int64(maxBytes)))
            errorMessage = nil
        } catch {
            errorMessage = "Failed to update cache limit: \(error)"
        }
    }

    private func clearCache() async {
        isClearingCache = true
        defer { isClearingCache = false }
        do {
            // Prune to 0 — evict everything currently stored.
            _ = try await state.pruneBlobs(targetBytes: 0)
            errorMessage = nil
        } catch {
            errorMessage = "Failed to clear cache: \(error)"
        }
    }

    private func openConfigInEditor() {
        let path = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".ctxfs/config.toml")
        NSWorkspace.shared.open(path)
    }
}
