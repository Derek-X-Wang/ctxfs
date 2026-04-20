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

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs/swift/ContextFS
rm -rf /tmp/ctxfs-sparkle-test
xcodebuild -project ContextFS.xcodeproj -scheme ContextFS \
           -configuration Release \
           -derivedDataPath /tmp/ctxfs-sparkle-test \
           CODE_SIGN_IDENTITY="-" CODE_SIGNING_REQUIRED=NO CODE_SIGNING_ALLOWED=NO
pkill -f "ContextFS.app/Contents/MacOS/ContextFS" 2>/dev/null || true
sudo rm -rf /Applications/ContextFS.app
cp -R /tmp/ctxfs-sparkle-test/Build/Products/Release/ContextFS.app /Applications/
```

### 2. Write a test appcast advertising a newer version

```bash
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
```

### 3. Serve the appcast locally

In one terminal:
```bash
cd /tmp/ctxfs-test-appcast
python3 -m http.server 8765
```

Leave it running. Verify it responds:
```bash
curl -s http://localhost:8765/appcast.xml | head -3
```

Expected: the XML content from Step 2.

### 4. Override SUFeedURL for one app launch

Use macOS `defaults` to override the feed URL. This writes to the app's
preferences, which Sparkle reads on launch and which takes precedence
over the Info.plist value.

```bash
defaults write ai.ctxfs.companion SUFeedURL "http://localhost:8765/appcast.xml"
```

### 5. Launch the app

```bash
open /Applications/ContextFS.app
```

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

```bash
defaults delete ai.ctxfs.companion SUFeedURL
```

### 8. Stop the local HTTP server

In the terminal from Step 3, press Ctrl+C.

## Failure modes

- **Menu item click does nothing:** `SparkleMenuAction` didn't initialize. Check Console.app for "SUUpdater" log lines — Sparkle logs init errors verbosely.
- **Dialog shows "You're up to date":** Version comparison failed. Ensure the test appcast uses `sparkle:version` 99 (we compare integer build versions, not semver strings).
- **Dialog says "update check failed":** The `defaults write` override didn't stick, or the HTTP server isn't running. Repeat Steps 3–4.
- **App crashes on launch:** Info.plist is malformed, or the `SUPublicEDKey` isn't valid base64. `plutil -lint /Applications/ContextFS.app/Contents/Info.plist` to check.
