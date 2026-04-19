import SwiftUI
import AppCore

// MARK: - Constants

/// Deep-link URL to the Login Items & Extensions pane in System Settings,
/// filtered to show FSKit filesystem module extensions.
private let fskitExtensionSettingsURL = URL(
    string: "x-apple.systempreferences:com.apple.LoginItems-Settings.extension?extensionPointIdentifier=com.apple.fskit.fsmodule"
)!

// MARK: - Step enum

enum OnboardingStep: Equatable {
    case welcome
    case quickExtension
    case quickToken
    case quickDone

    case customBackend
    case customExtension
    case customToken
    case customCache
    case customLaunchAtLogin
    case customDone

    var isTerminal: Bool { self == .quickDone || self == .customDone }
}

// MARK: - Root view

struct OnboardingView: View {
    @Bindable var state: DaemonState
    @Binding var isPresented: Bool
    @Environment(\.dismissWindow) private var dismissWindow

    @State private var step: OnboardingStep = .welcome
    @State private var githubToken: String = ""
    @State private var defaultBackend: BackendChoice = .auto
    @State private var cacheMaxMB: Double = 512
    @State private var launchAtLogin: Bool = false

    var body: some View {
        VStack(spacing: 24) {
            switch step {
            case .welcome:
                WelcomeStep(
                    onQuick: { step = .quickExtension },
                    onCustom: { step = .customBackend },
                    onSkip: finishAndDismiss
                )

            case .quickExtension, .customExtension:
                ExtensionStep(
                    state: state,
                    onContinue: {
                        step = (step == .quickExtension) ? .quickToken : .customToken
                    },
                    onBack: { step = (step == .quickExtension) ? .welcome : .customBackend }
                )
                .onChange(of: state.extensionStatus?.enabled) { _, newValue in
                    if newValue == true {
                        step = (step == .quickExtension) ? .quickToken : .customToken
                    }
                }

            case .quickToken, .customToken:
                TokenStep(
                    token: $githubToken,
                    state: state,
                    onContinue: {
                        persistToken()
                        step = (step == .quickToken) ? .quickDone : .customCache
                    },
                    onSkip: {
                        step = (step == .quickToken) ? .quickDone : .customCache
                    },
                    onBack: {
                        step = (step == .quickToken) ? .quickExtension : .customExtension
                    }
                )

            case .customBackend:
                BackendStep(
                    backend: $defaultBackend,
                    onContinue: {
                        persistBackend()
                        step = .customExtension
                    },
                    onBack: { step = .welcome }
                )

            case .customCache:
                CacheStep(
                    cacheMaxMB: $cacheMaxMB,
                    onContinue: {
                        persistCacheSize()
                        step = .customLaunchAtLogin
                    },
                    onBack: { step = .customToken }
                )

            case .customLaunchAtLogin:
                LaunchAtLoginStep(
                    enabled: $launchAtLogin,
                    onContinue: {
                        persistLaunchAtLogin()
                        step = .customDone
                    },
                    onBack: { step = .customCache }
                )

            case .quickDone, .customDone:
                DoneStep(onFinish: finishAndDismiss)
            }
        }
        .frame(width: 520, height: 380)
        .padding(24)
    }

    // MARK: - Completion

    private func finishAndDismiss() {
        UserDefaults.standard.set(true, forKey: UserDefaultsKey.onboardingComplete)
        isPresented = false
        dismissWindow(id: "onboarding")
    }

    // MARK: - Persist helpers

    private func persistToken() {
        guard !githubToken.isEmpty else { return }
        Task { try? await state.configSetValue(key: ConfigKey.githubToken, value: .string(githubToken)) }
    }

    private func persistBackend() {
        Task {
            try? await state.configSetValue(key: ConfigKey.backend, value: .string(defaultBackend.rawValue))
        }
    }

    private func persistCacheSize() {
        let bytes = Int64(cacheMaxMB) * 1_024 * 1_024
        Task {
            try? await state.configSetValue(key: ConfigKey.cacheMaxBytes, value: .int(bytes))
            _ = try? await state.setCacheLimits(maxBytes: UInt64(bytes))
        }
    }

    private func persistLaunchAtLogin() {
        // launch_at_login is SMAppService-only; not persisted in config.toml.
        _ = LoginItem.setEnabled(launchAtLogin)
    }
}

// MARK: - WelcomeStep

private struct WelcomeStep: View {
    let onQuick: () -> Void
    let onCustom: () -> Void
    let onSkip: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            RoundedRectangle(cornerRadius: 10)
                .fill(Color.accentColor)
                .frame(width: 48, height: 48)
                .overlay(
                    Text("C").font(.title).bold().foregroundStyle(.white)
                )

            Text("Welcome to ContextFS").font(.largeTitle.bold())
            Text("Mount Git repos and package sources as local directories. Let's get you set up.")
                .foregroundStyle(.secondary)

            Button(action: onQuick) {
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        HStack {
                            Text("Quick Setup").bold()
                            Text("RECOMMENDED")
                                .font(.caption2)
                                .padding(.horizontal, 8)
                                .padding(.vertical, 2)
                                .background(Color.accentColor)
                                .foregroundStyle(.white)
                                .cornerRadius(10)
                        }
                        Text("Enable the FSKit extension, add a GitHub token, done.")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    Spacer()
                }
                .padding()
                .background(RoundedRectangle(cornerRadius: 8).fill(Color.accentColor.opacity(0.15)))
            }
            .buttonStyle(.plain)

            Button(action: onCustom) {
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Custom Setup").bold()
                        Text("Walk through backend choice, cache size, and other options.")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    Spacer()
                }
                .padding()
                .background(RoundedRectangle(cornerRadius: 8).stroke(Color.secondary.opacity(0.5), lineWidth: 1))
            }
            .buttonStyle(.plain)

            Spacer()
            HStack {
                Spacer()
                Button("Skip for now", action: onSkip)
            }
        }
    }
}

// MARK: - ExtensionStep

private struct ExtensionStep: View {
    let state: DaemonState
    let onContinue: () -> Void
    let onBack: () -> Void

    private var enabled: Bool {
        state.extensionStatus?.enabled == true
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Enable the FSKit extension").font(.title2.bold())
            Text("ContextFS uses a macOS extension to mount filesystems without sudo. Enable it in System Settings once.")
                .foregroundStyle(.secondary)

            VStack(alignment: .leading, spacing: 6) {
                Label("Click \"Open System Settings\" below", systemImage: "1.circle")
                Label("Flip the toggle for contextfs to ON", systemImage: "2.circle")
                Label("Come back — we detect it automatically", systemImage: "3.circle")
            }
            .font(.callout)

            if enabled {
                Label("Extension is enabled!", systemImage: "checkmark.circle.fill")
                    .foregroundStyle(.green)
                    .font(.callout.bold())
            } else {
                Label("Waiting for you to enable the extension…", systemImage: "clock")
                    .foregroundStyle(.orange)
                    .font(.callout)
            }

            Spacer()

            HStack {
                Button("Back", action: onBack)
                Spacer()
                Button("Open System Settings", action: openSystemSettings)
                Button("Continue", action: onContinue)
                    .buttonStyle(.borderedProminent)
                    .disabled(!enabled)
            }
        }
    }

    private func openSystemSettings() {
        NSWorkspace.shared.open(fskitExtensionSettingsURL)
    }
}

// MARK: - TokenStep

private struct TokenStep: View {
    @Binding var token: String
    let state: DaemonState
    let onContinue: () -> Void
    let onSkip: () -> Void
    let onBack: () -> Void

    @State private var testResult: String = ""
    @State private var testing: Bool = false

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Add a GitHub token (optional)").font(.title2.bold())
            Text("Avoids API rate limits (60 req/hr unauthed → 5000 authed) and unlocks private repos.")
                .foregroundStyle(.secondary)

            SecureField("GitHub Personal Access Token", text: $token)
                .textContentType(.password)

            HStack {
                Button("Test") {
                    Task { await performTokenTest() }
                }
                .disabled(token.isEmpty || testing)
                if testing { ProgressView().controlSize(.small) }
                Text(testResult).font(.caption).foregroundStyle(.secondary)
            }

            Text("Create a token at github.com/settings/tokens (needs 'repo' scope).")
                .font(.caption).foregroundStyle(.tertiary)

            Spacer()

            HStack {
                Button("Back", action: onBack)
                Spacer()
                Button("Skip", action: onSkip)
                Button("Continue", action: onContinue)
                    .buttonStyle(.borderedProminent)
                    .disabled(token.isEmpty)
            }
        }
    }

    private func performTokenTest() async {
        testing = true
        defer { testing = false }
        do {
            let result = try await state.testGitHubToken(token: token)
            if result.valid {
                let remaining = result.remaining.map { "\($0) req remaining" } ?? ""
                let user = result.user.map { "user: \($0)" } ?? ""
                let parts = [user, remaining].filter { !$0.isEmpty }.joined(separator: ", ")
                testResult = "Valid (\(parts))"
            } else {
                testResult = "Invalid token"
            }
        } catch {
            testResult = "\(error.localizedDescription)"
        }
    }
}

// MARK: - BackendStep

private struct BackendStep: View {
    @Binding var backend: BackendChoice
    let onContinue: () -> Void
    let onBack: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Choose a default backend").font(.title2.bold())
            Text("You can override this per-mount with --backend.")
                .foregroundStyle(.secondary)

            Picker("Backend", selection: $backend) {
                ForEach(BackendChoice.allCases) { choice in
                    Text(choice.displayName).tag(choice)
                }
            }
            .pickerStyle(.radioGroup)

            Spacer()
            HStack {
                Button("Back", action: onBack)
                Spacer()
                Button("Continue", action: onContinue).buttonStyle(.borderedProminent)
            }
        }
    }
}

// MARK: - CacheStep

private struct CacheStep: View {
    @Binding var cacheMaxMB: Double
    let onContinue: () -> Void
    let onBack: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Cache size").font(.title2.bold())
            Text("How much disk ContextFS can use to cache fetched content.")
                .foregroundStyle(.secondary)

            Text("Maximum: \(Int(cacheMaxMB)) MB")
            Slider(value: $cacheMaxMB, in: 256...8192, step: 64)

            Spacer()
            HStack {
                Button("Back", action: onBack)
                Spacer()
                Button("Continue", action: onContinue).buttonStyle(.borderedProminent)
            }
        }
    }
}

// MARK: - LaunchAtLoginStep

private struct LaunchAtLoginStep: View {
    @Binding var enabled: Bool
    let onContinue: () -> Void
    let onBack: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Launch at login").font(.title2.bold())
            Text("Start ContextFS automatically when you log in. You can change this later in Preferences.")
                .foregroundStyle(.secondary)

            Toggle("Launch ContextFS at login", isOn: $enabled)
                .toggleStyle(.switch)

            Spacer()
            HStack {
                Button("Back", action: onBack)
                Spacer()
                Button("Continue", action: onContinue).buttonStyle(.borderedProminent)
            }
        }
    }
}

// MARK: - DoneStep

private struct DoneStep: View {
    let onFinish: () -> Void

    var body: some View {
        VStack(spacing: 16) {
            Image(systemName: "checkmark.circle.fill")
                .font(.system(size: 64))
                .foregroundStyle(.green)
            Text("You're all set").font(.largeTitle.bold())
            Text("ContextFS is ready to use. Open a terminal and try:\n\nctxfs mount github:octocat/Hello-World@master -p ./test")
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .padding(.horizontal)
            Spacer()
            Button("Done", action: onFinish).buttonStyle(.borderedProminent)
        }
    }
}
