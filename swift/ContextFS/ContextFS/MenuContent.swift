import SwiftUI
import AppCore

struct MenuContent: View {
    @Bindable var state: DaemonState
    @Binding var showPreferences: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            Divider()
            mountSection
            Divider()
            actionsSection
            Divider()
            footerSection
        }
        .frame(width: 320)
        .padding(.vertical, 4)
    }

    @ViewBuilder
    private var header: some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text("ContextFS").font(.headline)
                summaryLine
            }
            Spacer()
            StatusDot(state: state.iconState)
        }
        .padding(.horizontal)
        .padding(.vertical, 8)
    }

    @ViewBuilder
    private var summaryLine: some View {
        if !state.daemonRunning {
            Text("Daemon not running")
                .font(.caption)
                .foregroundStyle(.red)
        } else {
            let n = state.mounts.count
            let backend = state.mounts.first?.backend ?? inferredBackend
            Text("\(n) \(n == 1 ? "mount" : "mounts") · \(backend)")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    private var inferredBackend: String {
        state.extensionStatus?.enabled == true ? "fskit" : "nfs"
    }

    @ViewBuilder
    private var mountSection: some View {
        if state.mounts.isEmpty {
            VStack(alignment: .leading, spacing: 4) {
                Text("No active mounts")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Text("Use `ctxfs mount …` in your terminal.")
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
            }
            .padding(.horizontal)
            .padding(.vertical, 8)
        } else {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(state.mounts) { mount in
                    MountRow(mount: mount) {
                        Task { await state.unmount(mount.mountPoint) }
                    }
                }
            }
            .padding(.vertical, 4)
        }
    }

    @ViewBuilder
    private var actionsSection: some View {
        VStack(alignment: .leading, spacing: 0) {
            MenuActionButton("Unmount All", disabled: state.mounts.isEmpty) {
                Task { await state.unmountAll() }
            }
            MenuActionButton("Diagnostics…") {
                openDiagnostics()
            }
        }
        .padding(.vertical, 4)
    }

    @ViewBuilder
    private var footerSection: some View {
        VStack(alignment: .leading, spacing: 0) {
            MenuActionButton("Preferences…") {
                showPreferences = true
            }
            MenuActionButton("Quit ContextFS") {
                NSApplication.shared.terminate(nil)
            }
        }
        .padding(.vertical, 4)
    }

    // MARK: - Actions

    private func openDiagnostics() {
        // Placeholder — real diagnostics window is out of scope for 2b-B Task 6.
        // For now: show a simple NSAlert with the diag fields from state.
        // Task 7 wires this into the Preferences window or a dedicated sheet.
        let msg = """
        Daemon: \(state.daemonRunning ? "running" : "not running")
        Mounts: \(state.mounts.count)
        Extension: \(state.extensionStatus?.enabled == true ? "enabled" : "disabled")
        """
        let alert = NSAlert()
        alert.messageText = "ContextFS Diagnostics"
        alert.informativeText = msg
        alert.runModal()
    }
}

// MARK: - Subcomponents

private struct MountRow: View {
    let mount: MountInfo
    let onUnmount: () -> Void
    @State private var isHovered = false

    var body: some View {
        HStack(alignment: .top, spacing: 6) {
            Image(systemName: statusIcon)
                .foregroundStyle(statusColor)
                .frame(width: 12)
            VStack(alignment: .leading, spacing: 1) {
                Text(mount.source)
                    .font(.body)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text(mount.mountPoint)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            Spacer()
            if isHovered {
                Button(action: onUnmount) {
                    Image(systemName: "eject")
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
                .help("Unmount")
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 6)
        .contentShape(Rectangle())
        .background(isHovered ? Color.accentColor.opacity(0.15) : Color.clear)
        .onHover { isHovered = $0 }
    }

    private var statusIcon: String {
        switch mount.status {
        case .ready: return "checkmark.circle.fill"
        case .mounting: return "arrow.triangle.2.circlepath"
        case .unmounting: return "arrow.triangle.2.circlepath"
        case .error: return "exclamationmark.triangle.fill"
        case .unknown: return "questionmark.circle"
        }
    }

    private var statusColor: Color {
        switch mount.status {
        case .ready: return .green
        case .mounting, .unmounting: return .blue
        case .error: return .red
        case .unknown: return .secondary
        }
    }
}

private struct MenuActionButton: View {
    let title: String
    var disabled: Bool = false
    let action: () -> Void
    @State private var isHovered = false

    init(_ title: String, disabled: Bool = false, action: @escaping () -> Void) {
        self.title = title
        self.disabled = disabled
        self.action = action
    }

    var body: some View {
        Button(action: action) {
            HStack {
                Text(title)
                Spacer()
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 5)
            .contentShape(Rectangle())
            .background(isHovered && !disabled ? Color.accentColor.opacity(0.2) : Color.clear)
        }
        .buttonStyle(.plain)
        .disabled(disabled)
        .onHover { hovering in
            if !disabled { isHovered = hovering }
        }
    }
}

private struct StatusDot: View {
    let state: DaemonState.IconState

    var body: some View {
        if let color = dotColor {
            Circle()
                .fill(color)
                .frame(width: 8, height: 8)
        } else {
            EmptyView()
        }
    }

    private var dotColor: Color? {
        switch state {
        case .idle: return nil
        case .active: return .green
        case .setupNeeded: return .orange
        case .error: return .red
        case .busy: return .blue
        }
    }
}
