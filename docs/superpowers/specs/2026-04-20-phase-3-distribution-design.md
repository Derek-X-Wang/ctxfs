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
- **Keys**: generate one EdDSA keypair with `generate_keys` (ships with Sparkle). Private key stored in macOS Keychain **and** as GitHub Actions secret `SPARKLE_PRIVATE_KEY`. Public key embedded in `Info.plist` as `SUPublicEDKey`
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
- `conflicts_with formula: "contextfs"` — forbid side-by-side install with the CLI-only formula (both would symlink `ctxfs`)

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
- Ad-hoc signed — bare CLIs don't need Developer ID; Gatekeeper only gates `.app` bundles
- Tarball contents: `ctxfs` binary + `LICENSE` + minimal `README.md`. No man pages in Phase 3.
- `checksums.txt` alongside with SHA-256 of each tarball; `minisign`-signed for `ctxfs update` to verify

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
- **Install-path detection (safety rail)**: before self-updating, walk the parent dirs of `_NSGetExecutablePath()`. If any ancestor is `Cellar/`, `Caskroom/`, or `ContextFS.app`, refuse and print:
  - Homebrew formula/cask: `Run 'brew upgrade contextfs' instead`
  - Cask's bundled CLI: `This ctxfs is managed by ContextFS.app — update the app instead`

This prevents users from accidentally desyncing their package manager's view of the binary.

---

## Section 3 — GitHub Actions pipeline

### Job 1: `build-and-sign`

Trigger: `on: push: tags: 'v*.*.*'` — semver tags only.

Runner: `macos-latest`.

Steps:
1. Checkout
2. Install Rust toolchain with `aarch64-apple-darwin` + `x86_64-apple-darwin` targets
3. Read `VERSION` file → `$VERSION` env var
4. Assert `v$VERSION == $GITHUB_REF_NAME` (the tag, including `v` prefix) — abort on mismatch
5. `cargo build --release` for each target
6. `lipo -create -output ctxfs-universal …` for the .app embed; keep per-arch for tarballs
7. Import Developer ID cert from `DEVELOPER_ID_P12_BASE64` into a temp keychain (random unlock password, cleanup on job exit)
8. `xcodebuild -project ContextFS.xcodeproj -scheme ContextFS -configuration Release -allowProvisioningUpdates DEVELOPMENT_TEAM=RDQSC33B2X`
9. Re-sign `.app` + `.appex` with Developer ID (overrides Xcode's Apple Development sig)
10. `ditto -c -k --sequesterRsrc --keepParent ContextFS.app _notary_upload.zip` (intermediate zip for notarization upload — notarytool wants a zip, not a bare bundle)
11. `notarytool submit _notary_upload.zip --wait --timeout 30m`
12. `xcrun stapler staple ContextFS.app` (staple the app itself, not the zip; delete the intermediate zip)
13. Build DMG from stapled `.app` via `create-dmg` → `ContextFS-X.Y.Z.dmg`
14. Notarize + staple the DMG (same notarytool flow)
15. Create final `ContextFS-X.Y.Z.zip` (zip of the stapled `.app`, for Sparkle)
16. Package CLI tarballs, produce `checksums.txt`
17. `sparkle sign_update ContextFS-X.Y.Z.zip` → EdDSA signature written to a sidecar file
18. Pre-release validation (see below)
19. `gh release create --draft vX.Y.Z ContextFS-X.Y.Z.dmg ContextFS-X.Y.Z.zip ctxfs-X.Y.Z-darwin-arm64.tar.gz ctxfs-X.Y.Z-darwin-x86_64.tar.gz checksums.txt`

### Job 2: `publish-metadata`

Trigger: `on: release: types: [published]`.

Runner: `ubuntu-latest` (just file edits + Git).

Steps:
1. Checkout `gh-pages` branch
2. Generate new `<item>` XML block from the Release body + artifact URLs + EdDSA sig
3. Append to `appcast.xml`, commit, push to `gh-pages`
4. Clone `Derek-X-Wang/homebrew-ctxfs` using `HOMEBREW_TAP_PAT`
5. Rewrite `Casks/contextfs.rb` + `Formula/contextfs.rb` with new version, URLs, SHA-256s
6. `gh pr create` — one-click mergeable PR

### Why draft → manual publish → Job 2

The gap lets Derek download the draft DMG, install on a second Mac, sanity-check before anything reaches users. Mis-notarized or broken builds never reach the appcast or Homebrew.

### Pre-release validation (in Job 1)

1. `cargo clippy --all-targets --tests` — no warnings (inherits the `-D warnings` flag)
2. `cargo test` — all green
3. `./target/release/ctxfs --version` output matches `VERSION` file
4. `spctl -a -vv ContextFS.app` — Gatekeeper accepts
5. `codesign --verify --strict --deep ContextFS.app` — clean
6. `sparkle sign_update --verify` — EdDSA signature re-checks
7. Smoke test: unzip notarized `.app`, run `Contents/MacOS/ctxfs --version`, assert exit 0

### Secrets required

Set via repo Settings → Secrets:
- `DEVELOPER_ID_P12_BASE64` — Developer ID Application cert exported as .p12, base64'd
- `DEVELOPER_ID_P12_PASSWORD` — export password for the .p12
- `APPLE_ID` — Derek's Apple ID email
- `APPLE_ID_PASSWORD` — app-specific password (appleid.apple.com → Sign-In and Security → App-Specific Passwords)
- `APPLE_TEAM_ID` — `RDQSC33B2X`
- `SPARKLE_PRIVATE_KEY` — EdDSA private key from `generate_keys`
- `HOMEBREW_TAP_PAT` — fine-grained Personal Access Token scoped to `homebrew-ctxfs` repo with `contents: write` + `pull-requests: write`

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
5. Runs `cargo check` to update `Cargo.lock`
6. `git commit -am "chore: release vX.Y.Z"`
7. `git tag vX.Y.Z`

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
| Notarization rejects | Job 1 fails; notarytool log uploaded as workflow artifact; Derek debugs locally |
| Gatekeeper rejects stapled build (step 4 of pre-release) | Job 1 fails; usually means a nested binary wasn't re-signed — inspect with `codesign -dvvv` on each embedded binary |
| EdDSA signature verification fails | Job 1 fails; key misconfiguration — regenerate keys, re-run |
| Homebrew tap PR fails (merge conflict, permission denied) | Job 2 comments on the published Release with manual bump command; not blocking for Sparkle users |
| Appcast regeneration merge conflict on `gh-pages` | Job 2 retries once with `git pull --rebase origin gh-pages`; if still failing, fails loudly and Derek rebases manually |
| `VERSION` file doesn't match git tag | Job 1 aborts at step 4; user fixes mismatch and re-tags |
| Second-Mac sanity test surfaces a bug | Draft release is discarded; release script bumps patch version; re-releases |
| Sparkle private key in GitHub secrets gets leaked | Revoke via `generate_keys --revoke`, create new keypair, bump minor version (0.2.0), ship update with new `SUPublicEDKey`. Old installs auto-update one more time via old key, then are pinned to new key. Document in runbook. |

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
