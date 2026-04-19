import SwiftUI
import AppCore

/// Menu bar icon: a monochrome external-drive glyph with a small colored dot
/// overlay in the bottom-right corner.  The dot color reflects the daemon state:
/// - No dot  → idle (daemon up, no mounts)
/// - Green   → active (≥1 mount)
/// - Orange  → setup needed (FSKit not yet enabled)
/// - Red     → error (daemon unreachable)
/// - Blue    → busy (mount/unmount in progress)
struct StatusIcon: View {
    let state: DaemonState.IconState

    var body: some View {
        ZStack(alignment: .bottomTrailing) {
            Image(systemName: "externaldrive")
                .symbolRenderingMode(.monochrome)
            if let color = dotColor {
                Circle()
                    .fill(color)
                    .frame(width: 6, height: 6)
                    .overlay(
                        Circle()
                            .stroke(Color.black.opacity(0.3), lineWidth: 0.5)
                    )
                    .offset(x: 2, y: 2)
            }
        }
    }

    private var dotColor: Color? {
        state.statusDotColor
    }
}
