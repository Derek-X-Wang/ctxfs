# Phase 3a — Sparkle Integration Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Integrate Sparkle 2.7.x into the ContextFS Mac app so users can check for and receive updates via a menu bar "Check for Updates…" item, against an EdDSA-signed appcast feed.

**Architecture:** Sparkle is added as a SwiftPM dependency on the host `ContextFS` target only (the extension `ContextFSExt` does not update). An `SPUStandardUpdaterController` is owned by the App scene and passed down to `MenuContent`, which renders a new menu item that calls `updaterController.checkForUpdates(nil)` on tap. Info.plist gains the feed URL, public EdDSA key, and auto-check settings. The private EdDSA key stays out of the repo — it's generated locally, stored in the macOS Keychain, and later copied to a GitHub Actions secret during Phase 3e bootstrap.

**Tech Stack:**
- Sparkle 2.7.x via SwiftPM (`github.com/sparkle-project/Sparkle`)
- Swift 5.9+ (Xcode 17.x)
- macOS 15.4 deployment target (matches project)
- EdDSA signatures (Sparkle's modern signing, not DSA)

**What's out of scope for 3a** (belongs to later phases):
- EdDSA signing in CI (3d)
- Developer ID code signing in CI (3d)
- Real appcast hosted on gh-pages (3e)
- Full end-to-end update download (3e dress rehearsal)

3a's ship criterion: running the app on Derek's dev Mac, clicking "Check for Updates…", and seeing Sparkle's dialog respond against a **local test appcast** served from `http://localhost:8000/appcast.xml`.

---

## File structure

Files created or modified by this plan:

| File | Responsibility |
|---|---|
| `swift/ContextFS/ContextFS.xcodeproj/project.pbxproj` | Adds Sparkle as `XCRemoteSwiftPackageReference` + links `Sparkle` product to `ContextFS` target |
| `swift/ContextFS/ContextFS/Info.plist` | Adds `SUFeedURL`, `SUPublicEDKey`, `SUEnableAutomaticChecks`, `SUScheduledCheckInterval` |
| `swift/ContextFS/ContextFS/ContextFS.swift` | Owns `SPUStandardUpdaterController`, passes to `MenuContent` |
| `swift/ContextFS/ContextFS/MenuContent.swift` | Renders "Check for Updates…" menu item, dispatches to controller |
| `swift/ContextFS/ContextFS/SparkleMenuAction.swift` | New — thin SwiftUI-friendly wrapper around `SPUStandardUpdaterController` (makes controller non-optional and mockable) |
| `docs/phase3-sparkle-smoke-test.md` | New — one-page runbook for manual smoke test against local appcast (delete this doc when 3e ships) |

---

## Task 1: Add Sparkle as a SwiftPM dependency

**Files:**
- Modify: `swift/ContextFS/ContextFS.xcodeproj/project.pbxproj` (four sections: `XCRemoteSwiftPackageReference`, `XCSwiftPackageProductDependency`, `packageProductDependencies` of the `ContextFS` native target, and `packageReferences` of the project object)

Xcode edits the pbxproj for us when adding a package via the GUI. We're doing it manually so the plan is reproducible and diffs are reviewable. The engineer SHOULD prefer using Xcode's File → Add Package Dependencies… menu and committing the result — the tasks below document the expected diff so the engineer can verify Xcode produced the right output.

- [ ] **Step 1: Before touching the pbxproj, note the existing SwiftPM integration as the reference pattern**

Open `swift/ContextFS/ContextFS.xcodeproj/project.pbxproj` and locate the existing blocks for `swift-nio` (around line 585) and `swift-protobuf` (around line 593). These are the templates — Sparkle is added with the same structure.

Expected existing blocks to match format:
```
/* Begin XCRemoteSwiftPackageReference section */
		766207ED2E3AC43C00C75C6C /* XCRemoteSwiftPackageReference "swift-nio" */ = {
			isa = XCRemoteSwiftPackageReference;
			repositoryURL = "https://github.com/apple/swift-nio.git";
			requirement = {
				kind = upToNextMajorVersion;
				minimumVersion = 2.0.0;
			};
		};
```

- [ ] **Step 2: Open the Xcode project and add Sparkle via the GUI**

Run:
```bash
open /Users/derekxwang/Development/incubator/ContextFS/ctxfs/swift/ContextFS/ContextFS.xcodeproj
```

In Xcode:
1. File → Add Package Dependencies…
2. Search bar: `https://github.com/sparkle-project/Sparkle`
3. Dependency Rule: **Up to Next Major** → `2.7.0`
4. Click "Add Package"
5. On the product selection screen:
   - Check `Sparkle`
   - Target: **`ContextFS` only** (NOT `ContextFSExt`)
6. Click "Add Package"

Expected result: Xcode resolves the package (takes ~15 seconds), adds it to the project navigator under "Package Dependencies" showing `Sparkle`.

- [ ] **Step 3: Verify Xcode wrote the expected diff to pbxproj**

Run:
```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
git diff swift/ContextFS/ContextFS.xcodeproj/project.pbxproj | head -80
```

Expected: new `XCRemoteSwiftPackageReference` for `Sparkle`, new `XCSwiftPackageProductDependency` for the `Sparkle` product, and `Sparkle` added to `packageProductDependencies` of the ContextFS target. Nothing should change in `ContextFSExt`'s `packageProductDependencies`.

If Xcode also added entries under `ContextFSExt` (it sometimes does that by mistake), remove them — Sparkle must not link into the extension.

- [ ] **Step 4: Build to verify Sparkle resolves and links**

Run from the repo root:
```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs/swift/ContextFS
xcodebuild -project ContextFS.xcodeproj -scheme ContextFS -configuration Debug -derivedDataPath /tmp/ctxfs-sparkle-test 2>&1 | tail -3
```

Expected: `** BUILD SUCCEEDED **`.

If the build fails complaining about "no such module 'Sparkle'" after this task: Sparkle wasn't linked to the ContextFS target. Re-open Xcode → select the Sparkle package in project navigator → Frameworks & Libraries section of ContextFS target → ensure Sparkle is listed with "Required" embed option.

- [ ] **Step 5: Commit**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
git add swift/ContextFS/ContextFS.xcodeproj/project.pbxproj \
        swift/ContextFS/ContextFS.xcodeproj/project.xcworkspace/xcshareddata/swiftpm/Package.resolved
git commit -m "feat(app): add Sparkle 2.7.x SwiftPM dependency

Sparkle is linked to the ContextFS host target only; ContextFSExt
does not receive updates (the extension is updated in lockstep with
its host app). Integration wiring follows in subsequent tasks."
```

---

## Task 2: Generate the EdDSA keypair and populate Info.plist

The private key is stored in Derek's macOS Keychain (Sparkle's default storage location). The public key goes into Info.plist where Sparkle checks each downloaded update against it. The private key is NOT committed; it's copied to the `SPARKLE_PRIVATE_KEY` GitHub Actions secret during Phase 3e bootstrap.

**Files:**
- Modify: `swift/ContextFS/ContextFS/Info.plist`

- [ ] **Step 1: Download Sparkle's command-line tools**

Run:
```bash
cd /tmp
curl -L -o Sparkle-2.7.0.tar.xz \
  https://github.com/sparkle-project/Sparkle/releases/download/2.7.0/Sparkle-2.7.0.tar.xz
mkdir -p /tmp/sparkle-tools
tar -xJf Sparkle-2.7.0.tar.xz -C /tmp/sparkle-tools
ls /tmp/sparkle-tools/bin/
```

Expected output:
```
generate_appcast
generate_keys
old_dsa_scripts
sign_update
```

- [ ] **Step 2: Generate the EdDSA keypair**

```bash
/tmp/sparkle-tools/bin/generate_keys
```

Expected output (shape; actual key values differ):
```
A new signing key has been generated and saved in the Keychain.
...
This is the public key that you would add to your Info.plist file:
SUPublicEDKey = "AbCdEfGhIjKlMnOp...PQRSTUVWXYZ1234567890abcdef="
```

Copy the exact public key string printed. The private key is now in the login Keychain under service `https://sparkle-project.org`, account `ed25519`.

**Important:** if `generate_keys` says a key already exists and asks whether to replace — choose **no**. Use the existing key. (Running `generate_keys --replace` destroys the existing private key and would orphan any already-released apps. We're not at risk yet since we haven't shipped, but get in the habit.)

- [ ] **Step 3: Add the Sparkle keys to Info.plist**

Open `swift/ContextFS/ContextFS/Info.plist`. Current contents:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>NSSystemExtensionUsageDescriptionKey</key>
	<string>Activate FS Extension</string>
</dict>
</plist>
```

Replace with (paste the actual public key from Step 2 into the `SUPublicEDKey` string):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>NSSystemExtensionUsageDescriptionKey</key>
	<string>Activate FS Extension</string>
	<key>SUFeedURL</key>
	<string>https://derek-x-wang.github.io/ctxfs/appcast.xml</string>
	<key>SUPublicEDKey</key>
	<string>PASTE_PUBLIC_KEY_FROM_STEP_2_HERE</string>
	<key>SUEnableAutomaticChecks</key>
	<true/>
	<key>SUScheduledCheckInterval</key>
	<integer>86400</integer>
	<key>SUAllowsAutomaticUpdates</key>
	<false/>
</dict>
</plist>
```

Notes on each key:
- `SUFeedURL`: Where Sparkle fetches appcast.xml. This URL won't respond until Phase 3e bootstraps gh-pages. That's OK for 3a — smoke test uses a local override.
- `SUPublicEDKey`: The public half of the keypair generated in Step 2.
- `SUEnableAutomaticChecks = true`: Sparkle checks in the background; user can still trigger via menu.
- `SUScheduledCheckInterval = 86400`: Once per day (in seconds).
- `SUAllowsAutomaticUpdates = false`: Sparkle will prompt before applying. Don't silently auto-apply; the user wants agency.

- [ ] **Step 4: Verify Info.plist is well-formed**

```bash
plutil -lint /Users/derekxwang/Development/incubator/ContextFS/ctxfs/swift/ContextFS/ContextFS/Info.plist
```

Expected output: `swift/ContextFS/ContextFS/Info.plist: OK`

- [ ] **Step 5: Verify the public key is a valid base64 ed25519 key**

```bash
KEY=$(plutil -extract SUPublicEDKey raw /Users/derekxwang/Development/incubator/ContextFS/ctxfs/swift/ContextFS/ContextFS/Info.plist)
echo "$KEY" | base64 -d | wc -c
```

Expected: `32` (ed25519 public keys are 32 bytes).

If the byte count is not 32, the key was copied incorrectly — re-run `generate_keys` with `--output-public-key` to print just the key, or re-copy from Keychain Access.

- [ ] **Step 6: Commit**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
git add swift/ContextFS/ContextFS/Info.plist
git commit -m "feat(app): add Sparkle feed URL + EdDSA public key to Info.plist

Public key generated via Sparkle's generate_keys tool on 2026-04-20.
Private key lives in Derek's login Keychain (service
https://sparkle-project.org, account ed25519). Will be copied to
GitHub Actions secret SPARKLE_PRIVATE_KEY in Phase 3e.

Feed URL points at the gh-pages branch which doesn't respond yet;
Phase 3e bootstraps the appcast. Smoke-testing Sparkle locally
uses a temporary file:// or http://localhost override."
```

---

## Task 3: Create a SwiftUI-friendly wrapper for SPUStandardUpdaterController

`SPUStandardUpdaterController` is an Objective-C / AppKit class that doesn't compose cleanly with SwiftUI's environment-injection pattern. A thin wrapper gives us a testable, non-optional type to pass through the view hierarchy.

**Files:**
- Create: `swift/ContextFS/ContextFS/SparkleMenuAction.swift`

- [ ] **Step 1: Create the wrapper file**

Create `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/swift/ContextFS/ContextFS/SparkleMenuAction.swift` with this content:

```swift
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
```

- [ ] **Step 2: Verify the file compiles standalone**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs/swift/ContextFS
xcodebuild -project ContextFS.xcodeproj -scheme ContextFS -configuration Debug -derivedDataPath /tmp/ctxfs-sparkle-test 2>&1 | tail -3
```

Expected: `** BUILD SUCCEEDED **`.

If it fails with "no such module 'Sparkle'": confirm Task 1 completed — `Sparkle` must be linked to the `ContextFS` target.

- [ ] **Step 3: Commit**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
git add swift/ContextFS/ContextFS/SparkleMenuAction.swift
git commit -m "feat(app): add SparkleMenuAction wrapper around SPUStandardUpdaterController

Gives SwiftUI a non-optional, @MainActor-isolated entry point to
Sparkle's update-check flow without leaking AppKit types into the
view hierarchy."
```

---

## Task 4: Wire SparkleMenuAction into the App scene and pass to MenuContent

**Files:**
- Modify: `swift/ContextFS/ContextFS/ContextFS.swift`
- Modify: `swift/ContextFS/ContextFS/MenuContent.swift`

- [ ] **Step 1: Add the Sparkle action to ContextFSApp's init**

Open `swift/ContextFS/ContextFS/ContextFS.swift`. Find the `init()` method of `ContextFSApp`. The current version is:

```swift
@main
struct ContextFSApp: App {
    @State private var daemonState: DaemonState

    init() {
        let client: any HelperClientProtocol
        do {
            client = try HelperClient()
        } catch {
            client = NoopHelperClient(error: error)
        }

        let state = DaemonState(client: client)
        _daemonState = State(initialValue: state)

        if !LaunchdAgent.isInstalled {
            do {
                try LaunchdAgent.install()
            } catch {
                NSLog("ContextFS: LaunchdAgent install failed: \(error)")
            }
        }
        state.start()
    }
    // ...
}
```

Replace with this version — adds `sparkleAction` as a stored property and initializes it at the start of `init` so Sparkle can begin background checks as early as possible:

```swift
@main
struct ContextFSApp: App {
    @State private var daemonState: DaemonState
    private let sparkleAction: SparkleMenuAction

    init() {
        // Start Sparkle's background version checks before the daemon comes
        // up so the first scheduled check fires close to launch time.
        self.sparkleAction = SparkleMenuAction()

        let client: any HelperClientProtocol
        do {
            client = try HelperClient()
        } catch {
            client = NoopHelperClient(error: error)
        }

        let state = DaemonState(client: client)
        _daemonState = State(initialValue: state)

        if !LaunchdAgent.isInstalled {
            do {
                try LaunchdAgent.install()
            } catch {
                NSLog("ContextFS: LaunchdAgent install failed: \(error)")
            }
        }
        state.start()
    }
    // ...
}
```

- [ ] **Step 2: Pass sparkleAction down to MenuContent**

In the same file, find the `body` property of `ContextFSApp`. Current relevant slice:

```swift
    var body: some Scene {
        MenuBarExtra {
            MenuContent(state: daemonState, showPreferences: $showPreferences)
                .task {
                    if !UserDefaults.standard.bool(forKey: UserDefaultsKey.onboardingComplete) {
                        openWindow(id: "onboarding")
                        NSApp.activate(ignoringOtherApps: true)
                    }
                }
        } label: {
            StatusIcon(state: daemonState.iconState)
        }
        .menuBarExtraStyle(.window)
        // ...
    }
```

Modify the `MenuContent(…)` call to pass `sparkleAction`:

```swift
            MenuContent(
                state: daemonState,
                showPreferences: $showPreferences,
                sparkleAction: sparkleAction
            )
```

- [ ] **Step 3: Update MenuContent to accept sparkleAction and render the menu item**

Open `swift/ContextFS/ContextFS/MenuContent.swift`. Current top of the struct:

```swift
struct MenuContent: View {
    @Bindable var state: DaemonState
    @Binding var showPreferences: Bool
    @Environment(\.openWindow) private var openWindow
    // ...
```

Add the `sparkleAction` parameter:

```swift
struct MenuContent: View {
    @Bindable var state: DaemonState
    @Binding var showPreferences: Bool
    let sparkleAction: SparkleMenuAction
    @Environment(\.openWindow) private var openWindow
    // ...
```

Then find `footerSection`. Current:

```swift
    @ViewBuilder
    private var footerSection: some View {
        VStack(alignment: .leading, spacing: 0) {
            MenuActionButton("Preferences…") {
                openWindow(id: "preferences")
                NSApp.activate(ignoringOtherApps: true)
            }
            MenuActionButton("Quit") {
                NSApplication.shared.terminate(nil)
            }
        }
        .padding(.vertical, 4)
    }
```

Insert a "Check for Updates…" entry between Preferences and Quit:

```swift
    @ViewBuilder
    private var footerSection: some View {
        VStack(alignment: .leading, spacing: 0) {
            MenuActionButton("Preferences…") {
                openWindow(id: "preferences")
                NSApp.activate(ignoringOtherApps: true)
            }
            MenuActionButton("Check for Updates…") {
                sparkleAction.checkForUpdates()
            }
            MenuActionButton("Quit") {
                NSApplication.shared.terminate(nil)
            }
        }
        .padding(.vertical, 4)
    }
```

- [ ] **Step 4: Build the app**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs/swift/ContextFS
xcodebuild -project ContextFS.xcodeproj -scheme ContextFS -configuration Debug -derivedDataPath /tmp/ctxfs-sparkle-test 2>&1 | tail -3
```

Expected: `** BUILD SUCCEEDED **`.

If it fails with "missing argument for parameter 'sparkleAction'": a call site passing `MenuContent(...)` didn't get updated. Search for other `MenuContent(` calls (tests, previews) and pass a stub — there shouldn't be any besides the one in `ContextFS.swift`, but grep to confirm:

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
grep -rn "MenuContent(" swift/ContextFS/ContextFS/
```

- [ ] **Step 5: Commit**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
git add swift/ContextFS/ContextFS/ContextFS.swift \
        swift/ContextFS/ContextFS/MenuContent.swift
git commit -m "feat(app): wire 'Check for Updates…' menu item

SparkleMenuAction is owned by ContextFSApp, initialized early in
init() so background checks can start. MenuContent receives it as
a plain property and renders the menu item between Preferences
and Quit, matching Docker/Linear/Tailscale menu conventions."
```

---

## Task 5: Ensure hardened runtime is enabled

Sparkle requires the hardened runtime. Our current Release config may not have it enabled; our CI (Phase 3d) will also require it. Turn it on now so dev builds match the signing constraints we'll ship.

**Files:**
- Modify: `swift/ContextFS/ContextFS.xcodeproj/project.pbxproj`

- [ ] **Step 1: Check current state**

```bash
grep -n "ENABLE_HARDENED_RUNTIME\|CODE_SIGN_STYLE" /Users/derekxwang/Development/incubator/ContextFS/ctxfs/swift/ContextFS/ContextFS.xcodeproj/project.pbxproj | head -20
```

Expected: you'll see `ENABLE_HARDENED_RUNTIME` either missing (defaults to NO) or set to NO. We want `YES` for both Release config of both targets.

- [ ] **Step 2: Enable in Xcode**

In Xcode:
1. Select the `ContextFS` project (blue icon) in the navigator
2. Under TARGETS, select `ContextFS`
3. Build Settings tab
4. Search for "hardened"
5. Set **Enable Hardened Runtime** → `Yes` for both **Debug** and **Release**
6. Repeat for the `ContextFSExt` target

- [ ] **Step 3: Verify the pbxproj diff**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
git diff swift/ContextFS/ContextFS.xcodeproj/project.pbxproj | grep -E "HARDENED|^\+\+\+|^\-\-\-" | head -20
```

Expected: four new `ENABLE_HARDENED_RUNTIME = YES;` lines (Debug + Release × ContextFS + ContextFSExt = 4).

- [ ] **Step 4: Build to confirm hardened runtime doesn't break dev**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs/swift/ContextFS
xcodebuild -project ContextFS.xcodeproj -scheme ContextFS -configuration Debug -derivedDataPath /tmp/ctxfs-hardened-test 2>&1 | tail -3
```

Expected: `** BUILD SUCCEEDED **`.

Hardened runtime can break builds if the code does things like `dlopen` non-signed frameworks or calls disallowed syscalls. The ContextFS codebase doesn't do any of that, so the build should pass without entitlement additions.

- [ ] **Step 5: Commit**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
git add swift/ContextFS/ContextFS.xcodeproj/project.pbxproj
git commit -m "build(app): enable hardened runtime on both targets

Sparkle requires the hardened runtime to safely perform its XPC
installer dance. Developer ID signing in Phase 3d CI will also
require it. Enabling it now catches any hardened-runtime-breaking
code paths in local dev builds instead of first hitting them in CI."
```

---

## Task 6: Write a local smoke-test runbook

Without a real appcast we can't do end-to-end update testing — but we can verify Sparkle's menu dispatch and dialog rendering work. This task writes a short runbook so Derek (or a reviewer) can smoke-test the integration on a dev Mac.

**Files:**
- Create: `docs/phase3-sparkle-smoke-test.md`

- [ ] **Step 1: Create the runbook**

Create `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/docs/phase3-sparkle-smoke-test.md`:

```markdown
# Phase 3a Sparkle Smoke Test

Manual runbook. Verifies Sparkle's menu dispatch + dialog rendering work
against a local test appcast, without requiring the real gh-pages feed.

Delete this doc when Phase 3e's first real release lands — by then the
real appcast exists and this local-override dance is obsolete.

## Prerequisites

- Completed Phase 3a Tasks 1–5
- Python 3 available on PATH (`python3 --version`)
- The built `ContextFS.app` installed at `/Applications/ContextFS.app`

## Steps

### 1. Build + install a fresh .app

\`\`\`bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs/swift/ContextFS
rm -rf /tmp/ctxfs-sparkle-test
xcodebuild -project ContextFS.xcodeproj -scheme ContextFS \\
           -configuration Release \\
           -derivedDataPath /tmp/ctxfs-sparkle-test \\
           CODE_SIGN_IDENTITY="-" CODE_SIGNING_REQUIRED=NO CODE_SIGNING_ALLOWED=NO
pkill -f "ContextFS.app/Contents/MacOS/ContextFS" 2>/dev/null || true
sudo rm -rf /Applications/ContextFS.app
cp -R /tmp/ctxfs-sparkle-test/Build/Products/Release/ContextFS.app /Applications/
\`\`\`

### 2. Write a test appcast advertising a newer version

\`\`\`bash
mkdir -p /tmp/ctxfs-test-appcast
cat > /tmp/ctxfs-test-appcast/appcast.xml <<'EOF'
<?xml version="1.0" standalone="yes"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>ContextFS Test Updates</title>
    <item>
      <title>Version 99.0.0 (Test)</title>
      <sparkle:version>99</sparkle:version>
      <sparkle:shortVersionString>99.0.0</sparkle:shortVersionString>
      <description><![CDATA[<p>Local smoke test only; do not install.</p>]]></description>
      <pubDate>Sun, 20 Apr 2026 00:00:00 +0000</pubDate>
      <enclosure
        url="http://localhost:8765/nonexistent.zip"
        sparkle:edSignature="AA=="
        length="1"
        type="application/octet-stream"/>
    </item>
  </channel>
</rss>
EOF
\`\`\`

### 3. Serve the appcast locally

In one terminal:
\`\`\`bash
cd /tmp/ctxfs-test-appcast
python3 -m http.server 8765
\`\`\`

Leave it running. Verify it responds:
\`\`\`bash
curl -s http://localhost:8765/appcast.xml | head -3
\`\`\`

Expected: the XML content from Step 2.

### 4. Override SUFeedURL for one app launch

Use macOS `defaults` to override the feed URL. This writes to the app's
preferences, which Sparkle reads on launch and which takes precedence
over the Info.plist value.

\`\`\`bash
defaults write ai.ctxfs.companion SUFeedURL "http://localhost:8765/appcast.xml"
\`\`\`

### 5. Launch the app

\`\`\`bash
open /Applications/ContextFS.app
\`\`\`

Click the menu bar icon → click **Check for Updates…**.

### 6. Expected behavior

Sparkle displays its update dialog with these properties:
- Title: "A new version of ContextFS is available!"
- Version: 99.0.0
- Description: "Local smoke test only; do not install."
- Buttons: "Install Update" / "Skip This Version" / "Remind Me Later"

If you click **Install Update**, Sparkle will try to download the fake
zip from `http://localhost:8765/nonexistent.zip` and fail. That's fine —
the goal of this smoke test is just to verify the dialog-render path works.

Click **Skip This Version** to dismiss cleanly.

### 7. Reset the feed URL override

\`\`\`bash
defaults delete ai.ctxfs.companion SUFeedURL
\`\`\`

### 8. Stop the local HTTP server

In the terminal from Step 3, press Ctrl+C.

## Failure modes

- **Menu item click does nothing:** `SparkleMenuAction` didn't initialize. Check Console.app for "SUUpdater" log lines — Sparkle logs init errors verbosely.
- **Dialog shows "You're up to date":** Version comparison failed. Ensure the test appcast uses `sparkle:version` 99 (we compare integer build versions, not semver strings).
- **Dialog says "update check failed":** The `defaults write` override didn't stick, or the HTTP server isn't running. Repeat Steps 3–4.
- **App crashes on launch:** Info.plist is malformed, or the `SUPublicEDKey` isn't valid base64. `plutil -lint /Applications/ContextFS.app/Contents/Info.plist` to check.
```

- [ ] **Step 2: Commit the runbook**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
git add docs/phase3-sparkle-smoke-test.md
git commit -m "docs(phase3): Sparkle smoke-test runbook

Lets Derek (or a reviewer) validate the menu dispatch + dialog flow
against a local HTTP server + test appcast, without needing the real
gh-pages feed or signed release artifacts.

Runbook deletes itself once Phase 3e ships — the real appcast then
makes the local override dance unnecessary."
```

---

## Task 7: Run the smoke test end-to-end

Not automated — human validation. This is the ship gate for Plan 3a.

- [ ] **Step 1: Execute the runbook**

Follow every step in `docs/phase3-sparkle-smoke-test.md` exactly as written.

- [ ] **Step 2: Confirm the expected behavior from Step 6**

Sparkle dialog must render with version 99.0.0 and the three buttons. If not, treat as a regression and debug before calling the plan complete.

- [ ] **Step 3: Reset `defaults` and stop the HTTP server per Steps 7–8**

Leave the dev Mac in a clean state.

---

## Self-review checklist (already run; listing here for the executing engineer's awareness)

**Spec coverage:** Plan 3a covers spec Section 1 (Sparkle integration) fully — Sparkle framework, EdDSA key plan, Info.plist keys, "Check for Updates…" menu item. Spec Sections 3 (pipeline) and 6.3 (Sparkle key CI upload) are Phase 3d/3e, intentionally deferred. Spec Section 6.6 (build-rust.sh CI override) is Phase 3c.

**Placeholder scan:** No "TBD", "TODO", or "fill in" text in this plan. Every code block is complete. Every command has an expected output.

**Type consistency:** `SparkleMenuAction` is defined in Task 3 and referenced in Task 4 with the same public surface: `init()` + `checkForUpdates()`. `MenuContent`'s new parameter name `sparkleAction` is consistent across the init site (`ContextFS.swift`) and the struct definition.

**Known edges the plan does NOT solve** (all deferred to later phases with explicit citations above):
- No EdDSA signing of release zips (3d)
- No CI upload of the private key as a secret (3e)
- `SUFeedURL` won't respond until gh-pages is seeded (3e)
- Entitlements may need `com.apple.security.network.client` added when Sparkle actually tries to download — test will surface this in 3e dress rehearsal if so; local smoke test uses localhost which doesn't need this entitlement.
