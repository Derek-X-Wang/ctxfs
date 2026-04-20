import Foundation
import Sparkle

/// Thin SwiftUI-friendly wrapper around Sparkle's `SPUStandardUpdaterController`.
///
/// Sparkle's controller is an AppKit `NSObject`. This wrapper:
/// 1. Owns the controller's lifecycle (init at app startup).
/// 2. Exposes a single `checkForUpdates()` method the menu calls.
/// 3. Keeps Sparkle's types out of `MenuContent.swift`, which makes the
///    view easier to preview and doesn't drag AppKit into SwiftUI files.
@MainActor
final class SparkleMenuAction {
    private let updaterController: SPUStandardUpdaterController

    init() {
        // startingUpdater: true  → begin background version checks immediately.
        // updaterDelegate: nil   → accept Sparkle's default behavior.
        // userDriverDelegate: nil → accept Sparkle's default UI.
        self.updaterController = SPUStandardUpdaterController(
            startingUpdater: true,
            updaterDelegate: nil,
            userDriverDelegate: nil
        )
    }

    /// Trigger a user-visible update check. Called from the menu bar item.
    ///
    /// Shows Sparkle's dialog whether or not an update is available —
    /// this matches the "Check for Updates…" affordance in every Mac
    /// app that ships Sparkle.
    func checkForUpdates() {
        updaterController.checkForUpdates(nil)
    }
}
