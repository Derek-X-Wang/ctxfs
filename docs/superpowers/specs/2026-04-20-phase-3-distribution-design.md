# Phase 3 — Distribution Design Spec

**Date**: 2026-04-20
**Status**: Design validated via brainstorming session 2026-04-20.
**Scope**: Ship ContextFS as a distributable Mac app + CLI through Homebrew, GitHub Releases, and an in-app Sparkle updater. Soft-launch only — no marketing push; Derek dogfoods for 1–2 weeks before announcing publicly.

## Motivation

After Phase 2b-B, `ContextFS.app` works end-to-end on a dev-signed Mac. That's useful for the author and for anyone willing to build from source — nobody else. Phase 3 closes the loop so a new user can:

1. `brew install --cask contextfs` → full app, mount Git repos via FSKit
2. `brew install contextfs` → just the CLI, NFS backend, works in headless Macs / CI
3. Download a DMG from GitHub Releases → same experience as the cask, for the non-Homebrew crowd
4. Trust updates: the app auto-updates via Sparkle; the CLI updates via `brew upgrade` or `ctxfs update`

Distribution is the last missing link between "works on my machine" and "works on yours."

---

## Core Architectural Decisions

### 1. Soft-launch scope

Phase 3 builds the full pipeline but the *URLs* stay quiet until Derek has dogfooded on a second Mac for 1–2 weeks. The Homebrew tap exists from day one; it's just not announced. Version is `v0.1.0`, not `v1.0.0` — expectations honest.

### 2. Mac-only

Linux CLI binaries are out of scope. The CLI compiles on Linux today (CI verifies) but shipping Linux tarballs doubles the test matrix and the Homebrew path. Defer to Phase 3.5 when there's a concrete Linux user asking.

### 3. GitHub everything for hosting

- **Source**: `Derek-X-Wang/ctxfs`
- **Releases**: GitHub Releases on the same repo
- **Appcast XML**: GitHub Pages off the `gh-pages` branch of the same repo
- **Homebrew tap**: new repo `Derek-X-Wang/homebrew-ctxfs` (cask + formula)

No custom domain, no external hosting. A domain can CNAME in front of GitHub Pages later with zero code changes.

### 4. Tag-driven release via GitHub Actions, single version

A git tag `vX.Y.Z` is the only trigger for a release. CI builds, signs, notarizes, uploads, and regenerates the appcast — no manual `scp` dance. One version string flows from a root `VERSION` file into every artifact (Rust workspace, Swift `.app`, git tag, GitHub Release, Homebrew recipes).

### 5. Approach 1 scope — no `cargo install`

Skipping crates.io publishing for Phase 3. Requires publishing all 15 workspace crates + namespacing + public-API docs — ~1–2 days of busywork with no clear Phase 3 user. Add in Phase 3.5 when a Rust dev specifically asks.

---

## Architecture Overview

```
Developer
  └─ ./scripts/release.sh 0.1.0   (stamps version everywhere)
  └─ git push && git push --tags
           ↓
GitHub Actions — Job 1: build-and-sign (macos-latest, ~10–15 min)
  ├─ Build Rust workspace for arm64 + x86_64
  ├─ lipo binaries into universal for the .app embed
  ├─ Build Swift .app via xcodebuild
  ├─ Re-sign .app + .appex with Developer ID (from secrets)
  ├─ Notarize via notarytool, staple
  ├─ Package: ContextFS-X.Y.Z.zip (Sparkle) + .dmg (create-dmg)
  ├─ Package CLI: ctxfs-X.Y.Z-darwin-{arm64,x86_64}.tar.gz
  ├─ Compute EdDSA signature for the .zip
  └─ Create DRAFT GitHub Release with all artifacts attached
           ↓
Derek — manual validation (1–2 days)
  ├─ Install draft DMG on second Mac
  ├─ Run through mount + unmount smoke test
  └─ Click "Publish release" on GitHub when happy
           ↓
GitHub Actions — Job 2: publish-metadata (on release: published)
  ├─ Append <item> to appcast.xml on gh-pages branch
  └─ Open PR to Derek-X-Wang/homebrew-ctxfs bumping cask + formula
           ↓
Users install via:
  App:  brew install --cask contextfs  │  Sparkle auto-update  │  DMG download
  CLI:  brew install contextfs         │  ctxfs update          │  GH Releases tarball
```

**Repos involved:**
- `Derek-X-Wang/ctxfs` — source + CI + `gh-pages` branch hosting appcast.xml
- `Derek-X-Wang/homebrew-ctxfs` — new, cask + formula recipes
- GitHub Releases on `ctxfs` — hosts all binaries

---

## Section 1 — Mac app distribution

### Sparkle integration

- **Framework**: Sparkle 2.x via Swift Package Manager, added to the `ContextFS` target
- **Keys**: generate one EdDSA keypair with `generate_keys` (ships with Sparkle). Public key embedded in `Info.plist` as `SUPublicEDKey`. Private key lives in **two places**:
  - Derek's macOS Keychain (source of truth — used for emergency manual signing from a laptop)
  - GitHub Actions secret `SPARKLE_PRIVATE_KEY` (operational copy — used by CI for routine releases)
- **Blast radius:** if `SPARKLE_PRIVATE_KEY` leaks (e.g., a compromised GHA workflow), an attacker could publish updates that look legitimate to existing installs until we rotate. Acceptable risk given the Phase 3 threat model (solo dev, soft launch, ~dozens of users initially) but we name it explicitly, and Section 6 documents the key rotation runbook. If Phase 4+ brings thousands of users, this deserves a revisit — typical mitigation is a hardware key + manual release signing, or moving CI signing to a separate workload with short-lived credentials.
- **Info.plist additions**:
  - `SUFeedURL = https://derek-x-wang.github.io/ctxfs/appcast.xml`
  - `SUEnableAutomaticChecks = YES`
  - `SUScheduledCheckInterval = 86400` (daily)
- **UI**: add a "Check for Updates…" menu item between "Preferences…" and "Quit" in the existing menu bar dropdown
- **Update application**: Sparkle default — downloads in background, prompts the user, applies on next launch

### appcast.xml (on `gh-pages` branch)

- RSS-style XML, one `<item>` per release
- Each item contains: `<sparkle:version>`, `<sparkle:shortVersionString>`, `<enclosure url="…ContextFS-X.Y.Z.zip" sparkle:edSignature="…" length="…" type="application/octet-stream"/>`, release notes HTML inline from the GitHub Release body
- Job 2 regenerates this file by appending the new item and pushing to `gh-pages`

### DMG

- Built with `create-dmg` (Homebrew-available tool) after notarization completes
- Standard drag-to-install layout: background image + Applications symlink
- DMG itself is notarized + stapled so a clean Mac accepts it without a warning

### Homebrew cask

Location: `Derek-X-Wang/homebrew-ctxfs/Casks/contextfs.rb`

Key directives:
- `url` — DMG in the current GitHub Release
- `sha256` — DMG checksum
- `app "ContextFS.app"` — drag-install
- `binary "#{appdir}/ContextFS.app/Contents/MacOS/ctxfs"` — symlink the bundled CLI into `$HOMEBREW_PREFIX/bin/ctxfs` so `ctxfs` on PATH picks up the cask's binary
- `zap` stanza — on uninstall, remove `~/.ctxfs/`, `~/Library/LaunchAgents/ai.ctxfs.daemon.plist`, stale System Settings extension entries
- `conflicts_with formula: "contextfs"` — forbid side-by-side brew install with the CLI-only formula (both would symlink `ctxfs`)

**The cask-and-formula-and-DMG tri-state:** `conflicts_with` only covers the two-brew-install case. A user who drag-installs the DMG *and then* runs `brew install contextfs` would end up with the formula's `ctxfs` shadowing the cask's. Mitigations:

1. Cask install-time script checks for a prior non-brew `/Applications/ContextFS.app` and refuses until the user removes it (cask-managed vs. direct installs are distinguishable by the presence of `$(brew --prefix)/Caskroom/contextfs/`)
2. `ctxfs update` (running from either source) detects the brew formula's bin path via the canonicalization logic in Section 2 and refuses to self-update, directing the user to `brew upgrade`
3. The Preferences window in the companion app shows the running CLI path (`which ctxfs` equivalent) so users can see which copy is on PATH if they ask "why didn't the update land?"

Belt-and-suspenders. The normal path — either `brew install --cask contextfs` *or* DMG, plus optionally `brew install contextfs` for CLI on a headless Mac — Just Works.

### Notarization flow

- Uses `notarytool submit --wait` with:
  - `APPLE_ID` — Derek's Apple ID
  - `APPLE_ID_PASSWORD` — app-specific password (not the account password)
  - `APPLE_TEAM_ID` — `RDQSC33B2X`
- Hard timeout at 30 minutes; normal completion is 2–5 minutes
- On failure: CI uploads the notarytool log as a workflow artifact; user debugs manually (usually entitlement mismatch)
- Stapling happens after notarization succeeds, before zip/DMG creation

---

## Section 2 — CLI distribution

### GitHub Releases tarball (source of truth for non-cask CLI installs)

- Per-release: `ctxfs-X.Y.Z-darwin-arm64.tar.gz` and `ctxfs-X.Y.Z-darwin-x86_64.tar.gz`
- **Signed with Developer ID Application** + hardened runtime, `--options runtime --timestamp`. Not notarized — bare CLIs don't produce a notarizable archive format that Apple will staple to; Developer ID signing alone satisfies Gatekeeper *once the quarantine attribute is cleared*.
- Tarball contents: `ctxfs` binary + `LICENSE` + `README.md` with a "Direct download quarantine note" (see below). No man pages in Phase 3.
- `checksums.txt` alongside with SHA-256 of each tarball and every other Release artifact. `ctxfs update` verifies SHA-256 only. No `minisign` layer in Phase 3 — TLS + GitHub API authentication + SHA-256 is a strictly sufficient trust chain for the threat model, and managing an extra key without a clear threat it blocks is premature. If a future threat model ever calls for code-signed tarballs (e.g., offline verification), add `minisign` then with proper key lifecycle design.

### Quarantine handling for direct browser downloads

macOS attaches a `com.apple.quarantine` xattr to anything downloaded via a browser. For a Developer ID-signed but *not notarized* CLI, first-run prompts with "cannot verify developer." Two clean mitigations:

1. **Recommend Homebrew or `ctxfs update`** as the primary non-app install paths — neither adds a quarantine xattr.
2. **Document the one-time workaround** in the tarball README and the GitHub Release notes template: `xattr -d com.apple.quarantine ctxfs` after extracting. This is standard on the macOS-CLI-from-GitHub-Releases route (ripgrep, fd, bat users are all familiar).

Not a Phase 3 blocker — it's an expected trait of the direct-download channel for signed-but-not-notarized CLIs.

### Homebrew formula

Location: `Derek-X-Wang/homebrew-ctxfs/Formula/contextfs.rb`

- `on_arm { url … arm64 tarball; sha256 … }` / `on_intel { url … x86_64 tarball; sha256 … }`
- Installs `ctxfs` into `$HOMEBREW_PREFIX/bin/`
- `conflicts_with cask: "contextfs"` — reciprocal of the cask's conflict

### `ctxfs update` subcommand

- Built on the `self_update` crate (proven by ripgrep, fd, sccache)
- Queries `api.github.com/repos/Derek-X-Wang/ctxfs/releases/latest`, compares `tag_name` to `env!("CARGO_PKG_VERSION")`
- If a newer version exists:
  1. Download the tarball matching current platform (`uname -m`)
  2. Verify SHA-256 against `checksums.txt`
  3. Atomically swap `$(which ctxfs)` → new binary
  4. Print release notes snippet + "updated to vX.Y.Z; restart your shell sessions"
- `ctxfs update --check` exits 0 if up-to-date, 1 if newer available — for scripting
- **Install-path detection (safety rail)**: before self-updating, resolve the running binary's path deterministically:
  1. Call `_NSGetExecutablePath` to get the invocation path
  2. `std::fs::canonicalize` it — resolves every symlink including `$HOMEBREW_PREFIX/bin/ctxfs` → `$HOMEBREW_PREFIX/Cellar/contextfs/<ver>/bin/ctxfs` (formula), **or** → `/Applications/ContextFS.app/Contents/MacOS/ctxfs` (cask or DMG)
  3. Check in order of specificity:
     a. **Inside the app bundle (cask *or* direct DMG install):** canonical path contains `/Applications/ContextFS.app/Contents/MacOS/` — refuse with a single message that covers both cases:
        `This ctxfs is bundled with ContextFS.app. Update via the app's 'Check for Updates…' menu (or 'brew upgrade --cask contextfs' if you installed via Homebrew).`
        We intentionally do **not** try to distinguish cask-managed from DMG-managed here — canonical path alone can't tell them apart, and the user-facing instruction covers both cleanly.
     b. **Homebrew formula:** canonical path contains `$(brew --prefix)/Cellar/contextfs/` (shell out to `brew --prefix` once, or read `HOMEBREW_PREFIX` env var if set) — refuse with `Run 'brew upgrade contextfs' instead`
     c. **Neither:** proceed with self-update

The `canonicalize` + `$(brew --prefix)` pair handles both Apple Silicon (`/opt/homebrew`) and Intel (`/usr/local`) prefixes deterministically, avoids the "symlink masks manager ownership" trap Codex flagged, and doesn't rely on parent-directory name matching.

This prevents users from accidentally desyncing their package manager's view of the binary.

---

## Section 3 — GitHub Actions pipeline

### Design principles (driving decisions below)

1. **Sign once, deterministically, inside Xcode.** No "dev-sign then re-sign" dance — that's where nested-binary signing goes wrong. Use `CODE_SIGN_STYLE=Manual` with Developer ID Application cert + explicit provisioning profiles for both targets. Xcode handles ordering correctly when given the inputs it expects.
2. **Explicit nested-signing pass after Xcode, as a verification sweep.** Xcode signs in the right order; we re-verify + sign anything Xcode missed (Rust helpers embedded by the pre-build script, Sparkle framework resources) with `--options runtime --timestamp` before the outer-app hash is computed. `codesign --deep` is deprecated (per Apple); we walk the bundle explicitly.
3. **Universal build is produced before Xcode runs.** `build-rust.sh` is modified to respect a `CTXFS_PREBUILT_RUST_DIR` env var — if set, it copies from that dir instead of invoking cargo. CI builds universal Rust binaries first, sets the env var, then runs xcodebuild. Dev builds (no env var) keep compiling normally.
4. **Every artifact consumed by Job 2 is a Release asset or Release body field.** No implicit "file on the runner" handoff. Job 2 downloads what it needs via `gh release download` from the same `tag_name`.

### Pinned tooling

Pinned with explicit versions so builds are reproducible. Upgraded deliberately via PR, not silently:

| Tool | Version | Install command |
|---|---|---|
| macOS runner image | `macos-14` (not `macos-latest`) | runs-on: macos-14 |
| Xcode | 17.x via `xcode-select` | `sudo xcode-select -s /Applications/Xcode_17.app` |
| Rust | `rust-toolchain.toml` at repo root | auto-selected by rustup |
| `create-dmg` | `1.2.x` pinned | `brew install create-dmg` with a post-check `brew info --json create-dmg` asserting major version |
| Sparkle tools (`sign_update`, `generate_appcast`) | Sparkle `2.7.x` | Downloaded from `github.com/sparkle-project/Sparkle/releases` to a cache-keyed runner dir; SHA-256 pinned in the workflow file |
| `gh` CLI | pre-installed on runner | no action |
| `ditto`, `codesign`, `notarytool`, `stapler`, `lipo` | Xcode-bundled | no action |

Tool cache hit-rate is a bonus; pinning is the requirement.

### Job 1: `build-and-sign`

Trigger: `on: push: tags: 'v*.*.*'` — semver tags only.

Runner: `macos-14`.

Permissions:
```yaml
permissions:
  contents: write   # for gh release create
```

Tokens used:
- `GITHUB_TOKEN` (auto) — for `gh release create` against this repo

Steps (step numbers are meaningful — they're referenced in the error table):

1. `actions/checkout@v4` with `fetch-depth: 0` (needed for `git rev-list --count HEAD` in Section 4)
2. `sudo xcode-select -s /Applications/Xcode_17.app`
3. `dtolnay/rust-toolchain@stable` with targets: `aarch64-apple-darwin, x86_64-apple-darwin`
4. Install + version-pin `create-dmg` via brew; download Sparkle tool bundle, verify SHA-256 against workflow-pinned value
5. Read `VERSION` file → `$VERSION` env var; assert `v$VERSION == $GITHUB_REF_NAME` (abort on mismatch)
6. Import Developer ID cert:
   - `echo "$DEVELOPER_ID_P12_BASE64" | base64 -d > /tmp/cert.p12`
   - `security create-keychain -p $(openssl rand -hex 16) build.keychain`
   - `security default-keychain -s build.keychain`
   - `security unlock-keychain -p $KEYCHAIN_PW build.keychain`
   - `security import /tmp/cert.p12 -k build.keychain -P "$DEVELOPER_ID_P12_PASSWORD" -T /usr/bin/codesign -T /usr/bin/productbuild`
   - `security set-key-partition-list -S apple-tool:,apple: -s -k $KEYCHAIN_PW build.keychain`
   - `rm /tmp/cert.p12`
   - Register a cleanup trap to delete the keychain on job exit regardless of success
7. Install provisioning profiles:
   - `echo "$DEVELOPER_ID_APP_PROFILE_BASE64" | base64 -d > ~/Library/MobileDevice/Provisioning\ Profiles/contextfs_app.provisionprofile`
   - `echo "$DEVELOPER_ID_EXT_PROFILE_BASE64" | base64 -d > ~/Library/MobileDevice/Provisioning\ Profiles/contextfs_ext.provisionprofile`
   - Each profile embeds the FSKit Module capability (exported once manually during bootstrap — see Section 6)
8. Build universal Rust:
   - `cargo build --release --target aarch64-apple-darwin -p ctxfs -p ctxfs-app-helper`
   - `cargo build --release --target x86_64-apple-darwin -p ctxfs -p ctxfs-app-helper`
   - `lipo -create …arm64/ctxfs …x86_64/ctxfs -output /tmp/universal/ctxfs`
   - `lipo -create …arm64/ctxfs-app-helper …x86_64/ctxfs-app-helper -output /tmp/universal/ctxfs-app-helper`
9. Export `CTXFS_PREBUILT_RUST_DIR=/tmp/universal` — `build-rust.sh` will pick this up and skip its own `cargo build`, copying these universal binaries instead.
10. xcodebuild (deterministic signing — no `-allowProvisioningUpdates`):
    ```
    xcodebuild -project swift/ContextFS/ContextFS.xcodeproj \
      -scheme ContextFS -configuration Release \
      -derivedDataPath /tmp/ctxfs-build \
      CODE_SIGN_STYLE=Manual \
      DEVELOPMENT_TEAM=RDQSC33B2X \
      CODE_SIGN_IDENTITY="Developer ID Application: Xinzhe Wang (RDQSC33B2X)" \
      OTHER_CODE_SIGN_FLAGS="--options runtime --timestamp"
    ```
    **Per-target `PROVISIONING_PROFILE_SPECIFIER` is set in `pbxproj`, not on the xcodebuild command line.** `xcodebuild`'s `key=value` build-setting overrides apply globally to every target — there's no supported `KEY_TargetName=value` syntax, so the extension and host app can't be given different profiles that way. The bootstrap task (Section 6.1) edits `project.pbxproj` once to set:
    - `ContextFS` target, Release config → `PROVISIONING_PROFILE_SPECIFIER = ContextFS Distribution`
    - `ContextFSExt` target, Release config → `PROVISIONING_PROFILE_SPECIFIER = ContextFS Extension Distribution`

    These settings are committed to the repo. CI's xcodebuild invocation then needs no profile flags — Xcode reads them from the project. Profile names must match the embedded `name` field of the `.provisionprofile` files created in Section 6.1. If those names change (e.g., portal rename), commit the new values to pbxproj and the `DEVELOPER_ID_*_PROFILE_BASE64` secrets together.

    (`vars.PROVISIONING_PROFILE_*_NAME` workflow variables from the earlier draft are removed — they created a second source of truth that would drift from pbxproj.)
11. Verify Xcode's signing handled everything, and add explicit sigs to any nested Rust binaries that may have been embedded post-sign:
    - For each in [`Contents/MacOS/ctxfs`, `Contents/MacOS/ctxfs-app-helper`]: `codesign --force --sign "Developer ID Application: Xinzhe Wang (RDQSC33B2X)" --options runtime --timestamp --identifier ai.ctxfs.companion.$(basename) "$f"`
    - Re-sign the outer `.app` with `--force` so the outer hash covers the new nested signatures
    - Run `codesign --verify --strict --verbose=4 ContextFS.app` — fail on any "invalid signature" output
12. Notarize the app:
    - `ditto -c -k --sequesterRsrc --keepParent ContextFS.app /tmp/_notary_app.zip`
    - `xcrun notarytool submit /tmp/_notary_app.zip --wait --timeout 30m --apple-id "$APPLE_ID" --password "$APPLE_ID_PASSWORD" --team-id "$APPLE_TEAM_ID"`
    - `xcrun stapler staple ContextFS.app`
    - `rm /tmp/_notary_app.zip`
13. Build DMG from the stapled `.app`:
    - `create-dmg --volname "ContextFS" --background swift/ContextFS/resources/dmg-bg.png --window-size 500 300 --icon "ContextFS.app" 125 150 --app-drop-link 375 150 ContextFS-$VERSION.dmg ContextFS.app`
14. Sign the DMG with Developer ID, then notarize + staple:
    - `codesign --force --sign "Developer ID Application: Xinzhe Wang (RDQSC33B2X)" --options runtime --timestamp ContextFS-$VERSION.dmg`
    - Same notarytool + stapler flow as step 12
15. Create the Sparkle update archive (zip of stapled .app — this is what auto-update downloads):
    - `ditto -c -k --sequesterRsrc --keepParent ContextFS.app ContextFS-$VERSION.zip`
16. Sign + package CLI binaries:
    - For each arch in [arm64, x86_64]: `codesign --force --sign "Developer ID Application: Xinzhe Wang (RDQSC33B2X)" --options runtime --timestamp target/<arch>-apple-darwin/release/ctxfs`
    - Create `ctxfs-$VERSION-darwin-arm64.tar.gz` + `ctxfs-$VERSION-darwin-x86_64.tar.gz` with binary + LICENSE + minimal README
    - CLI tarballs are **not** notarized — bare CLIs don't produce a notarizable bundle format, and Developer ID signing alone satisfies Gatekeeper *after the quarantine attribute is cleared*. See Section 2 for how we handle browser-download quarantine.
17. Compute checksums:
    - `cd release-artifacts && shasum -a 256 *.tar.gz *.dmg *.zip > checksums.txt`
18. Sign the Sparkle update zip:
    - Sparkle 2.x's `sign_update` tool reads the private key from the user's Keychain by default. In CI we don't have a populated keychain, so use the `--ed-key-file` flag with the key material piped from the GH secret, and capture the printed signature string into the sidecar file:
      ```bash
      # SPARKLE_PRIVATE_KEY is the base64 ed25519 private key as produced by generate_keys
      echo "$SPARKLE_PRIVATE_KEY" > /tmp/sparkle_priv.b64
      SIG=$(sign_update ContextFS-$VERSION.zip --ed-key-file /tmp/sparkle_priv.b64)
      echo "$SIG" > ContextFS-$VERSION.zip.sig
      rm -f /tmp/sparkle_priv.b64  # shred isn't in macOS toolchain
      ```
      The sidecar is plain text containing one signature string (`sparkle:edSignature` attribute value + length metadata), not the raw key.
    - The `.sig` file is a **first-class release asset** — Job 2 downloads it by this filename, no implicit handoff.
19. Pre-release validation (see subsection below). Fails the job on any red check.
20. Create draft GitHub Release:
    - `gh release create vX.Y.Z --draft --title "v$VERSION" --notes-file .github/release-notes/vX.Y.Z.md ContextFS-$VERSION.dmg ContextFS-$VERSION.zip ContextFS-$VERSION.zip.sig ctxfs-$VERSION-darwin-arm64.tar.gz ctxfs-$VERSION-darwin-x86_64.tar.gz checksums.txt`
    - Release notes file is committed to the repo at `.github/release-notes/vX.Y.Z.md` before tagging — Derek writes it during the version bump. CI fails at this step if the file is absent (which is the desired behavior — no un-annotated releases).

### Job 2: `publish-metadata`

Triggers (both supported — second is for recovery):
```yaml
on:
  release:
    types: [published]
  workflow_dispatch:
    inputs:
      tag:
        description: "Release tag to reconcile (e.g. v0.1.0)"
        required: true
```

Runner: `ubuntu-latest`.

Permissions:
```yaml
permissions:
  contents: write   # for gh-pages push on this repo
  issues: write     # for opening failure issues on this repo (step 12)
```

Tokens used (annotated per step below):
- **`GITHUB_TOKEN`** (auto-provisioned by Actions) — scoped to the current repo (`Derek-X-Wang/ctxfs`). Used for: downloading release assets, pushing to `gh-pages`, creating issues.
- **`HOMEBREW_TAP_PAT`** (secret) — cross-repo PAT scoped to `Derek-X-Wang/homebrew-ctxfs`. Used for: cloning the tap, pushing a branch, opening the bump PR.

Steps:
1. Resolve tag: either `${{ github.event.release.tag_name }}` or `${{ github.event.inputs.tag }}` *(auth: none — context read)*
2. `gh release download $TAG --pattern 'ContextFS-*.zip.sig' --pattern 'ContextFS-*.dmg' --pattern 'ctxfs-*.tar.gz' --pattern 'checksums.txt'` *(auth: `GITHUB_TOKEN`)*
3. Parse `.sig` file to extract the EdDSA signature + length (local processing, no auth)
4. Parse `checksums.txt` to get SHA-256 of each artifact (local processing, no auth)
5. Generate new `<item>` XML block: version, shortVersionString, enclosure URL (pointing at `github.com/Derek-X-Wang/ctxfs/releases/download/$TAG/…`), EdDSA sig in `sparkle:edSignature`, release notes HTML from the tagged release body (escaped via `xmlstarlet` or Python's `xml.sax.saxutils.escape` — not raw string concat)
6. Checkout `gh-pages` branch (orphan-safe with `--depth=1`) *(auth: `GITHUB_TOKEN` for checkout)*
7. Validate existing appcast.xml is well-formed (`xmllint --noout`); if the file doesn't exist, initialize it from the Phase 3 bootstrap template (see Section 6)
8. Insert new `<item>` at the top of `<channel>` (newest first, per Sparkle convention), re-validate, commit, push to `gh-pages` *(auth: `GITHUB_TOKEN` — requires `contents: write`)*
9. Clone `Derek-X-Wang/homebrew-ctxfs` using `HOMEBREW_TAP_PAT`, create a branch `bump-v$VERSION` *(auth: `HOMEBREW_TAP_PAT`)*
10. Rewrite `Casks/contextfs.rb` + `Formula/contextfs.rb` — version string, 3 URLs (DMG, arm64 tarball, x86_64 tarball), 3 SHA-256s. Regenerate using a Python script in the repo at `scripts/render-homebrew.py` so the edits are deterministic (local processing, no auth)
11. Push the branch; `gh pr create --repo Derek-X-Wang/homebrew-ctxfs --head bump-v$VERSION --title "Bump contextfs to v$VERSION" --body-file …` *(auth: `HOMEBREW_TAP_PAT`)*
12. On any failure in steps 9–11 (PAT expired, merge conflict, etc.): open a GitHub Issue on this repo so Derek isn't silently left out-of-sync. Compose the run URL from documented env vars (not the fictional `$GITHUB_JOB_URL`):
    ```bash
    RUN_URL="${GITHUB_SERVER_URL}/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}"
    gh issue create --repo Derek-X-Wang/ctxfs \
      --title "Tap bump failed for v$VERSION" \
      --body "Job 2 failed at step 9-11. Workflow run: $RUN_URL"
    ```
    *(auth: `GITHUB_TOKEN` — requires `issues: write`)*
    Tap sync being slow isn't user-facing (Sparkle is the faster path), but we want a visible backlog, not a silent one.

### Why draft → manual publish → Job 2

The gap lets Derek download the draft DMG, install on a second Mac, sanity-check before anything reaches users. Mis-notarized or broken builds never reach the appcast or Homebrew. If Job 2 itself fails after publish (rare but possible), `workflow_dispatch` replay path makes recovery a one-click operation rather than a code edit.

### Pre-release validation (in Job 1, between steps 18 and 20)

1. `cargo clippy --all-targets --tests` — no warnings (inherits the `-D warnings` flag)
2. `cargo test` — all green
3. `./target/aarch64-apple-darwin/release/ctxfs --version` and `./target/x86_64-apple-darwin/release/ctxfs --version` both match `VERSION` file
4. `spctl -a -vv ContextFS.app` — Gatekeeper accepts
5. `spctl -a -vv --type install ContextFS-$VERSION.dmg` — DMG also accepts
6. `codesign --verify --strict --verbose=4 ContextFS.app` — clean (no `--deep`; deprecated)
7. For each nested binary in `ContextFS.app/Contents/MacOS/` and `ContextFS.app/Contents/Extensions/*.appex`: `codesign -dvvv` showing Developer ID Application identity and a hardened runtime flag
8. Verify the Sparkle signature round-trips. `sign_update --verify` takes the signature *string* as an argument, not a file path, so the sidecar must be read first:
   ```bash
   sign_update --verify "$(cat ContextFS-$VERSION.zip.sig)" ContextFS-$VERSION.zip
   ```
9. Smoke test: unzip notarized `.app` to `/tmp/`, run `/tmp/ContextFS.app/Contents/MacOS/ctxfs --version`, assert exit 0 and output matches `VERSION`

### Secrets required

Set via repo Settings → Secrets:

**Apple signing/notarization:**
- `DEVELOPER_ID_P12_BASE64` — Developer ID Application cert exported as .p12, base64'd
- `DEVELOPER_ID_P12_PASSWORD` — export password for the .p12
- `DEVELOPER_ID_APP_PROFILE_BASE64` — `ai.ctxfs.companion` provisioning profile (with FSKit Module capability), base64'd
- `DEVELOPER_ID_EXT_PROFILE_BASE64` — `ai.ctxfs.companion.fskitext` provisioning profile (with FSKit Module capability), base64'd
- `APPLE_ID` — Derek's Apple ID email
- `APPLE_ID_PASSWORD` — app-specific password (appleid.apple.com → Sign-In and Security → App-Specific Passwords)
- `APPLE_TEAM_ID` — `RDQSC33B2X`

**Sparkle:**
- `SPARKLE_PRIVATE_KEY` — EdDSA private key from Sparkle's `generate_keys` tool. See Section 6 for rotation & emergency procedures.

**Cross-repo:**
- `HOMEBREW_TAP_PAT` — fine-grained Personal Access Token scoped to `homebrew-ctxfs` repo with `contents: write` + `pull-requests: write`. 90-day expiry reminder in Derek's calendar.

**No workflow variables needed** — provisioning profile specifier names are committed to `project.pbxproj` per target (see Section 3 step 10 and Section 6.1). Single source of truth.

---

## Section 4 — Versioning

### Single source of truth

New file `VERSION` at repo root, containing exactly `0.1.0\n` (no `v` prefix).

### Release script

New script `scripts/release.sh X.Y.Z`:

1. Writes `X.Y.Z` to `VERSION`
2. Updates root `Cargo.toml` `workspace.package.version` (new field — currently per-crate versions are separate)
3. Updates each `crates/*/Cargo.toml` to `version.workspace = true` (migration from `version = "0.0.0"` during Phase 3)
4. Updates `swift/ContextFS/ContextFS.xcodeproj/project.pbxproj`:
   - `MARKETING_VERSION` → `X.Y.Z`
   - `CURRENT_PROJECT_VERSION` → `$(git rev-list --count HEAD)` at the time of bump. Monotonic, offline-computable, standard pattern.
5. Runs `cargo generate-lockfile --offline` (or falls back to `cargo update -p <each workspace crate>` if the version bump changes path deps) to refresh `Cargo.lock` deterministically. Plain `cargo check` is not a reliable lockfile-refresh mechanism — it only touches the lock if something triggers resolution, which a version-string bump on path deps may not.
6. Asserts `.github/release-notes/vX.Y.Z.md` exists and is non-empty — aborts with a clear message if Derek forgot to write it. (Derek writes it before running the release script; the script just guards against skipping the step.)
7. Stages + commits every touched path explicitly. `-am` would miss the newly-created release-notes file, which Job 1 step 20 (`gh release create --notes-file …`) then fails on:
   ```bash
   git add VERSION Cargo.toml Cargo.lock \
           crates/*/Cargo.toml \
           swift/ContextFS/ContextFS.xcodeproj/project.pbxproj \
           .github/release-notes/vX.Y.Z.md
   git commit -m "chore: release vX.Y.Z"
   ```
8. `git tag vX.Y.Z`

Derek runs the script, reviews the commit + tag, then `git push && git push --tags` manually (no auto-push — last chance to catch mistakes).

---

## Section 5 — Testing & error handling

### Pre-release validation

Automated in Job 1 (see Section 3). A red check on any step aborts before a draft Release is created.

### Post-release manual validation (Derek's job)

After Job 1 completes:
1. Download the draft DMG from the GitHub Release page
2. Install on primary Mac — verify mount/unmount/cache commands work
3. Install on a second Mac (clean, no dev tools) — verify Gatekeeper accepts the DMG, app launches, FSKit extension registers, mount succeeds
4. If anything breaks: do not publish. Fix, bump patch version (`0.1.0` → `0.1.1`), re-run the release script, new draft replaces old

### Error handling table

| Failure | Behavior |
|---|---|
| `VERSION` vs tag mismatch (Job 1 step 5) | Job aborts before any expensive work; Derek re-runs the release script cleanly |
| Keychain import (step 6) or profile import (step 7) fails | Job aborts; usually a secret/base64 encoding issue — validate the exported `.p12` and `.provisionprofile` locally, rotate as needed |
| Rust universal build (step 8) fails on either target | Job aborts; rust-toolchain targets may be out of sync with MSRV — update `rust-toolchain.toml` |
| xcodebuild (step 10) rejects the provisioning profile | Job aborts; most likely the profile specifier name doesn't match the embedded name, or the profile expired — re-export from developer.apple.com (Section 6 bootstrap doc) |
| Nested-binary verification (step 11) reports "invalid signature" | Job aborts; indicates a binary was embedded after Xcode signed the outer — usually `build-rust.sh` misbehaving (check that `CTXFS_PREBUILT_RUST_DIR` was read) |
| Notarization (step 12 or 14) rejects | Job fails; notarytool log URL printed in workflow output + uploaded as workflow artifact; Derek debugs locally (usually entitlement mismatch or timestamp server timeout) |
| Gatekeeper (`spctl`) rejects stapled build (pre-release step 4 or 5) | Job fails; inspect every nested binary with `codesign -dvvv` for missing hardened runtime or untrusted certs |
| Sparkle signature round-trip (pre-release step 8) fails | Job fails; `SPARKLE_PRIVATE_KEY` secret is corrupted or mismatched with embedded public key — regenerate pair (see Section 6 rotation runbook) |
| `gh release create` (step 20) fails | Job fails; most likely `GITHUB_TOKEN` permissions missing `contents: write` — verify workflow `permissions:` block |
| Appcast validation (Job 2 step 7) fails | Job fails; existing `appcast.xml` on gh-pages is malformed — manual rebase/fix, then `workflow_dispatch` replay |
| Homebrew tap PR (Job 2 steps 9–11) fails | Job 2 opens a GitHub Issue on the main ctxfs repo with the failed tag and workflow run URL. Sparkle users already got their update via Job 2 steps 6–8; Homebrew users will just see the update one release cycle late, which is fine. Derek fixes the tap manually. |
| Second-Mac sanity test (manual) surfaces a bug | Draft release is discarded; release script bumps patch version; re-releases. Never publish a draft you don't trust. |
| Sparkle private key leak | See Section 6 rotation runbook. Summary: revoke, generate new keypair, ship a `v0.X+1.0` update signed with the **old** key that also embeds the new `SUPublicEDKey`, then future updates use the new key. Existing installs auto-update once via the old key and are then pinned to the new. |
| Job 2 needs re-running (appcast got stuck, tap PR conflict resolved manually, etc.) | `workflow_dispatch` trigger with the tag as input re-runs Job 2 idempotently — dedupes by tag, doesn't create duplicate appcast items, upserts the tap PR |

---

---

## Section 6 — One-time bootstrap (pre-first-release)

Done once, by Derek, before `v0.1.0` ships. Committed as `docs/phase3-bootstrap-runbook.md` in the repo so it's reproducible if the laptop disappears.

### 6.1 Apple Developer portal setup

1. Sign in at `developer.apple.com/account`, confirm team `RDQSC33B2X` is active.
2. Register App IDs (Certificates, Identifiers & Profiles):
   - `ai.ctxfs.companion` with capability: App Groups (optional), Hardened Runtime. No FSKit Module on the host.
   - `ai.ctxfs.companion.fskitext` with capability: **FSKit Module**, Hardened Runtime.
3. Create provisioning profiles (both Developer ID Distribution type, not development):
   - `ContextFS Distribution` → App ID `ai.ctxfs.companion`, cert = Developer ID Application
   - `ContextFS Extension Distribution` → App ID `ai.ctxfs.companion.fskitext`, cert = Developer ID Application, includes FSKit Module capability
4. Download both `.provisionprofile` files. **Record the exact `name` field of each profile** — those strings are committed to `project.pbxproj` as each target's `PROVISIONING_PROFILE_SPECIFIER` build setting (see Section 3 step 10 for why this must live in pbxproj, not as a workflow-time override).

### 6.2 Export secrets into GitHub Actions

One shell script `scripts/bootstrap-secrets.sh` that Derek runs locally. Prompts for inputs, emits `gh secret set` commands (doesn't execute them — Derek reviews + runs manually).

```bash
# Outputs the commands Derek runs to populate GitHub secrets
./scripts/bootstrap-secrets.sh \
  --p12 ~/Downloads/developer-id.p12 \
  --p12-password-stdin \
  --app-profile ~/Downloads/ContextFS_Distribution.provisionprofile \
  --ext-profile ~/Downloads/ContextFS_Extension_Distribution.provisionprofile \
  --apple-id derek@... \
  --team-id RDQSC33B2X
```

Script does: base64-encodes each file, emits `gh secret set DEVELOPER_ID_P12_BASE64 < file.base64` commands to stdout. Derek pipes to a file, reviews, executes.

Renewal reminder: `.provisionprofile` files expire after ~1 year. Add a calendar reminder.

### 6.3 Sparkle key generation

```bash
cd /tmp && curl -L -o Sparkle.tar.xz https://github.com/sparkle-project/Sparkle/releases/download/2.7.x/Sparkle-2.7.x.tar.xz
tar xf Sparkle.tar.xz
./bin/generate_keys  # stores private key in login keychain, prints public key to stdout
```

- Private key is stored by `generate_keys` in the user's **login keychain** as a generic password. Sparkle 2.x uses service name `https://sparkle-project.org` and account name `ed25519` (verified against Sparkle's `generate_keys` source — see `github.com/sparkle-project/Sparkle/blob/2.x/generate_keys/main.swift`).
- Public key is printed by `generate_keys` to stdout; goes into `Info.plist` as `SUPublicEDKey` during Phase 3 implementation.
- Extract the private key into GitHub Actions secret `SPARKLE_PRIVATE_KEY`:
  ```bash
  # Correct account is 'ed25519' (not 'ed_private_key'); -w prints the base64 key to stdout
  security find-generic-password -s "https://sparkle-project.org" -a "ed25519" -w | gh secret set SPARKLE_PRIVATE_KEY
  ```
  If the `find-generic-password` command can't find the item, double-check the exact service/account strings by inspecting the keychain entry in Keychain Access (look for "sparkle" in the search box).

### 6.4 Homebrew tap repo

1. Create `Derek-X-Wang/homebrew-ctxfs` on GitHub (public repo)
2. Seed with stub `Casks/contextfs.rb` and `Formula/contextfs.rb` pointing at a non-existent `v0.0.0` — replaced by Job 2's first PR
3. Add README explaining tap usage: `brew tap Derek-X-Wang/ctxfs && brew install --cask contextfs`
4. Create `HOMEBREW_TAP_PAT` at `github.com/settings/tokens?type=beta`:
   - Resource owner: Derek-X-Wang
   - Repositories: `Derek-X-Wang/homebrew-ctxfs` only
   - Permissions: Contents: Read & Write, Pull Requests: Read & Write
   - Expiration: 90 days; set a calendar reminder to rotate

### 6.5 gh-pages branch seed

```bash
cd ctxfs && git checkout --orphan gh-pages
git rm -rf .
cat > appcast.xml <<EOF
<?xml version="1.0" standalone="yes"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>ContextFS Updates</title>
    <link>https://derek-x-wang.github.io/ctxfs/appcast.xml</link>
    <description>Updates for ContextFS.app</description>
    <language>en</language>
    <!-- items appended by CI Job 2; newest first -->
  </channel>
</rss>
EOF
cat > index.html <<EOF
<!doctype html>
<html><body><h1>ContextFS</h1><p>See <a href="https://github.com/Derek-X-Wang/ctxfs">the repo</a>.</p></body></html>
EOF
git add appcast.xml index.html
git commit -m "chore: seed gh-pages for Sparkle appcast"
git push -u origin gh-pages
```

Then in repo Settings → Pages: source = `gh-pages` branch, `/` root. First deploy completes in ~1 minute. Confirm `https://derek-x-wang.github.io/ctxfs/appcast.xml` loads.

### 6.6 Modify `build-rust.sh`

Add env-var fast-path so CI can inject prebuilt universal binaries:

```bash
# Inserted near top of build-rust.sh
if [ -n "${CTXFS_PREBUILT_RUST_DIR:-}" ]; then
    echo "[build-rust.sh] using prebuilt Rust from $CTXFS_PREBUILT_RUST_DIR (CI mode)"
    DEST="${BUILT_PRODUCTS_DIR}/${PRODUCT_NAME}.app/Contents/MacOS"
    mkdir -p "$DEST"
    cp -f "$CTXFS_PREBUILT_RUST_DIR/ctxfs" "$DEST/ctxfs"
    cp -f "$CTXFS_PREBUILT_RUST_DIR/ctxfs-app-helper" "$DEST/ctxfs-app-helper"
    exit 0
fi
# Existing cargo build logic follows...
```

Dev builds (no env var) work exactly as today. CI builds get the universal output without a second cargo invocation.

### 6.7 First release dress rehearsal

Before pushing `v0.1.0`:

1. Push `v0.0.1-rc1` as a test tag — same pipeline runs, creates a draft release Derek can poke
2. Verify: draft exists with all 5 artifacts, Gatekeeper accepts the DMG, Sparkle verifies the sig
3. Delete the draft + tag; don't publish
4. Only then push real `v0.1.0`

This catches pipeline misconfigurations before they'd pollute the public release history.

### 6.8 Key rotation runbook

**Sparkle EdDSA key rotation (on leak or precautionary annual rotation):**

1. Generate new keypair: `./bin/generate_keys --replace` (Sparkle tool writes new key; old one moved aside)
2. Update `SPARKLE_PRIVATE_KEY` secret in GitHub Actions
3. Embed the **new** public key in Info.plist as `SUPublicEDKey` + keep the **old** public key in Info.plist as `SUPublicEDKeys` (plural, array) for one release cycle — Sparkle 2.x supports this for seamless rollover
4. Release `v0.X+1.0` signed with the *old* private key (existing installs trust it); this release's Info.plist has both keys
5. Release `v0.X+2.0` signed with the *new* key; remove the old public key from Info.plist
6. Existing installs auto-update through this path without any user action

**Developer ID cert rotation (expiry or revocation):**
- Standard Apple Developer portal flow: revoke old, create new, re-export `.p12`, update `DEVELOPER_ID_P12_BASE64` secret. Provisioning profiles tied to the old cert will stop working; regenerate them bound to the new cert.

---

## Out of Scope (explicit NOs, not "maybe later")

- **`cargo install ctxfs` / crates.io publishing** — Phase 3.5 when a Rust dev asks
- **Linux binaries** — Phase 3.5
- **Windows** — not planned
- **Sparkle delta updates** — full-app download is fine for a <50 MB bundle
- **`curl | sh` shell installer** — `ctxfs update` covers non-Homebrew users after first install
- **Analytics, telemetry, crash reporting** — separate future discussion
- **Automated release note generation** — Derek writes them in the GitHub Release body manually (markdown renders in both GitHub UI and Sparkle's update dialog)
- **Auto-check-for-updates on the CLI** — `ctxfs update` is explicit/manual only; the app has Sparkle for the GUI audience
- **Rollback mechanism** — none. If a release ships broken, bump patch version and release the fix. Sparkle will pull users forward on next check. Homebrew users get the fix via `brew upgrade`.
- **Signed shell completions / man pages in the tarball** — nice-to-have, Phase 3.5
- **Universal DMG with optional components** — single DMG ships the full app
