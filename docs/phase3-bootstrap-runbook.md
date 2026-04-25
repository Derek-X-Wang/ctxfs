# Phase 3 — bootstrap runbook

**Audience:** future Derek (or a successor) re-bootstrapping the Phase 3 distribution pipeline from a clean Mac and a fresh GitHub clone.

This document is the authoritative answer to "the laptop dies / I rotate Apple accounts / a maintainer joins — how do I rebuild every external dependency the release pipeline relies on?" It is **not** the implementation plan (`docs/superpowers/plans/2026-04-20-phase-3e-bootstrap-first-release.md`); the plan was an agent-driven checklist run once. This runbook is the steady-state reference, with the lessons we paid for in 19 dress-rehearsal CI iterations baked in.

The pipeline ships:

- A **signed + notarized `ContextFS.app`** (DMG via Homebrew Cask, in-app updates via Sparkle 2.9.x EdDSA-signed feeds)
- A **signed CLI tarball** (Homebrew Formula on Apple Silicon + Intel; self-update via `ctxfs update`)

It depends on **eight external systems** that all need to be in place before any tag push will succeed.

---

## 0. Preflight — fresh-machine checks

Before touching Apple Developer or GitHub, confirm the local toolchain. Skip this section if you're continuing on the same laptop you bootstrapped on.

```bash
gh auth status                              # Logged in to github.com as Derek-X-Wang
gh api repos/Derek-X-Wang/ctxfs --silent    # 200 OK
xcode-select -p                             # /Applications/Xcode.app/Contents/Developer
xcodebuild -version                         # Xcode 26.x or later
brew --version                              # any 5.x+
security list-keychains                     # login.keychain-db present
git config user.email                       # matches your Apple ID where possible
```

If `xcodebuild` is older than 26, the project's pbxproj `objectVersion = 90` will not parse. Update Xcode first.

---

## 1. Apple Developer portal — App IDs, cert, profiles

This is the irreducibly-web step. Requires an active **paid Apple Developer Program** membership under team `RDQSC33B2X` (Xinzhe Wang). Without it nothing else in this runbook applies.

### 1.1 App IDs

Visit `https://developer.apple.com/account/resources/identifiers/list` → **+** → **App IDs**.

Create two App IDs:

| Identifier | Capabilities |
|---|---|
| `ai.ctxfs.companion` | Hardened Runtime |
| `ai.ctxfs.companion.fskitext` | Hardened Runtime + **FSKit Module** |

**Critical:** the FSKit Module capability must be checked on the extension App ID. If the option doesn't appear in your portal, contact Apple Developer support — it has been region-rolled out incrementally. Without this, notarization rejects the bundled extension every release.

### 1.2 Developer ID Application certificate

You should already have one if you've done dev builds. Verify in Keychain Access (My Certificates) or via:

```bash
security find-identity -v -p codesigning | grep "Developer ID Application: Xinzhe Wang"
```

If absent, generate a new one in the portal: **Certificates** → **+** → **Developer ID Application** → follow the CSR flow (Keychain Access → Certificate Assistant → Request a Certificate From a Certificate Authority).

These certs are valid for **5 years**. Note the SHA-1 fingerprint — pick the cert with the **longest-remaining expiry** when binding profiles in step 1.3, and use that same cert for the `.p12` export in step 2.

### 1.3 Provisioning profiles

Visit `https://developer.apple.com/account/resources/profiles/list` → **+**.

Create two **Developer ID Distribution** (under the Distribution heading — not Development) profiles, both **Direct** type (not "Managed by Xcode"):

| Profile name (exact) | App ID | Cert |
|---|---|---|
| `ContextFS Distribution` | `ai.ctxfs.companion` | the cert from 1.2 |
| `ContextFS Extension Distribution` | `ai.ctxfs.companion.fskitext` | same cert |

Download both `.provisionprofile` files. The string after "Provisioning Profile Name" is committed verbatim to `swift/ContextFS/ContextFS.xcodeproj/project.pbxproj` as each Release config's `PROVISIONING_PROFILE_SPECIFIER` — copy-paste the names to avoid typos.

**Lesson learned:** Developer ID profiles get an **~18-year expiry** bound to the cert lifetime, not the ~1-year expiry App Store profiles have. The cert (~5 years) is the short-lived artifact; set your rotation reminder against the cert's expiry, not the profile's.

### 1.4 Per-target `PROVISIONING_PROFILE_SPECIFIER` in pbxproj

xcodebuild's command-line `KEY=value` overrides apply project-wide, not per-target. Each target's profile name has to live in pbxproj:

```
ContextFS target Release config:        PROVISIONING_PROFILE_SPECIFIER = "ContextFS Distribution";
ContextFSExt target Release config:     PROVISIONING_PROFILE_SPECIFIER = "ContextFS Extension Distribution";
```

Already committed; verify with `grep PROVISIONING_PROFILE_SPECIFIER swift/ContextFS/ContextFS.xcodeproj/project.pbxproj`.

**Lesson learned:** the Release config used to also have `"CODE_SIGN_IDENTITY[sdk=macosx*]" = "Apple Development";`. The `[sdk=macosx*]` variant is more specific than the `xcodebuild CODE_SIGN_IDENTITY="Developer ID..."` command-line override and silently won. xcodebuild then signed Release builds with the development cert, which auto-includes the `get-task-allow` entitlement, which Apple's notary service rejects every time. Either drop the line entirely (current state) or set it to `"Developer ID Application"`.

---

## 2. GitHub Actions secrets

The release workflow needs nine secrets. Set them via `gh secret set` rather than the web UI so you can keep the originals only in Keychain.

### 2.1 Export the Developer ID `.p12`

In Keychain Access (login keychain → My Certificates):

1. Right-click the **Developer ID Application: Xinzhe Wang (RDQSC33B2X)** cert with the longest-remaining expiry
2. **Export "Developer ID Application…"** → format **Personal Information Exchange (.p12)**
3. Save to `~/Downloads/ctxfs-developer-id.p12`, set a password (record in your password manager as `ctxfs-ci-p12-password`)

### 2.2 Generate an app-specific password for `notarytool`

1. https://account.apple.com/account/manage → **Sign-In and Security** → **App-Specific Passwords**
2. Generate, label `ctxfs-ci`, copy the 16-char `abcd-efgh-ijkl-mnop` value

### 2.3 Set the secrets

Four are non-sensitive and can be piped directly:

```bash
base64 -i ~/Downloads/ctxfs-developer-id.p12 | gh secret set DEVELOPER_ID_P12_BASE64 --repo Derek-X-Wang/ctxfs
base64 -i ~/Downloads/ContextFS_Distribution.provisionprofile | gh secret set DEVELOPER_ID_APP_PROFILE_BASE64 --repo Derek-X-Wang/ctxfs
base64 -i ~/Downloads/ContextFS_Extension_Distribution.provisionprofile | gh secret set DEVELOPER_ID_EXT_PROFILE_BASE64 --repo Derek-X-Wang/ctxfs
printf '%s' 'RDQSC33B2X' | gh secret set APPLE_TEAM_ID --repo Derek-X-Wang/ctxfs
```

Three are passwords — don't paste into shell history. Use `read -s`:

```bash
read -s -p "DEVELOPER_ID_P12_PASSWORD: " PW && echo
printf '%s' "$PW" | gh secret set DEVELOPER_ID_P12_PASSWORD --repo Derek-X-Wang/ctxfs
unset PW

read -s -p "APPLE_ID (email): " PW && echo
printf '%s' "$PW" | gh secret set APPLE_ID --repo Derek-X-Wang/ctxfs
unset PW

read -s -p "APPLE_ID_PASSWORD (app-specific): " PW && echo
printf '%s' "$PW" | gh secret set APPLE_ID_PASSWORD --repo Derek-X-Wang/ctxfs
unset PW
```

Two more in stages 3 (`SPARKLE_PRIVATE_KEY`) and 4 (`HOMEBREW_TAP_PAT`).

### 2.4 Shred local copies

```bash
rm -P ~/Downloads/ctxfs-developer-id.p12
```

The `.provisionprofile` files aren't secret (they're config); leaving them in `~/Downloads` is fine.

`gh secret list --repo Derek-X-Wang/ctxfs` should now show 7 entries.

---

## 3. Sparkle EdDSA key

### 3.1 Verify the keypair exists in Keychain

Phase 3a generated this once. Re-running `generate_keys` regenerates the key, which **invalidates every shipped Sparkle update for existing installs** — only do that under the rotation procedure (section 8 below).

```bash
security find-generic-password -s "https://sparkle-project.org" -a "ed25519" 2>&1 | head -5
```

If absent, the keypair was destroyed (laptop wipe). Either restore from a Keychain backup or follow rotation in section 8.

### 3.2 Push the private key to GitHub

```bash
security find-generic-password -s "https://sparkle-project.org" -a "ed25519" -w | \
  gh secret set SPARKLE_PRIVATE_KEY --repo Derek-X-Wang/ctxfs
```

Keychain Access prompts for permission. Approve. The `-w` flag prints the raw base64-encoded 32-byte seed to stdout, which `gh` reads from stdin and uploads. The key never lands on disk in plaintext.

**Lesson learned:** Sparkle 2.9's `sign_update` CLI looks in the keychain by default. CI doesn't have a keychain entry — it has a secret env var. The canonical pattern is to pipe via stdin: `printf '%s' "$KEY" | sign_update --ed-key-file - <file>`. The `--ed-key-file <path>` form works too but is fragile to whitespace differences. `release.yml` uses the stdin form.

### 3.3 Pin the Sparkle tarball SHA-256

`release.yml` downloads the Sparkle CLI tools tarball during each run. The version + SHA-256 are pinned as workflow env vars; bumping requires both.

To recompute when bumping:

```bash
SPARKLE_VERSION=2.9.1   # match Xcode's SPM-resolved version (Package.resolved)
curl -fsSL -o /tmp/Sparkle.tar.xz \
  "https://github.com/sparkle-project/Sparkle/releases/download/${SPARKLE_VERSION}/Sparkle-${SPARKLE_VERSION}.tar.xz"
shasum -a 256 /tmp/Sparkle.tar.xz | awk '{print $1}'
```

Update `SPARKLE_VERSION` and `SPARKLE_TARBALL_SHA256` in `.github/workflows/release.yml` env block. Keep these aligned with the Sparkle framework version Xcode resolves via SPM (`Package.resolved`) — minor-version mismatch works (sign_update from any 2.x signs any 2.x bundle) but full alignment removes a class of subtle runtime bugs.

---

## 4. Homebrew tap repo

A separate public repo serves the cask + formula. CI Job 2 (`publish-metadata.yml`) auto-bumps it on every release.

### 4.1 Create the repo

Once. If it already exists (most cases), skip to 4.3.

```bash
gh repo create Derek-X-Wang/homebrew-ctxfs \
  --public \
  --description "Homebrew tap for ContextFS (auto-maintained)" \
  --license MIT \
  --add-readme
```

### 4.2 Seed with stub recipes

So Job 2's first PR has something to diff against:

```bash
gh repo clone Derek-X-Wang/homebrew-ctxfs /tmp/homebrew-ctxfs-bootstrap
cd /tmp/homebrew-ctxfs-bootstrap
mkdir -p Casks Formula

cat > Casks/contextfs.rb <<'EOF'
# STUB: populated by Derek-X-Wang/ctxfs publish-metadata.yml on first release.
cask "contextfs" do
  version "0.0.0"
  sha256 :no_check
  url "https://example.invalid/placeholder"
  name "ContextFS"
  desc "ContextFS (placeholder until first release lands)"
  homepage "https://github.com/Derek-X-Wang/ctxfs"
  app "ContextFS.app"
end
EOF

cat > Formula/contextfs.rb <<'EOF'
# STUB: populated by Derek-X-Wang/ctxfs publish-metadata.yml on first release.
class Contextfs < Formula
  desc "ContextFS (placeholder until first release lands)"
  homepage "https://github.com/Derek-X-Wang/ctxfs"
  url "https://example.invalid/placeholder"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  version "0.0.0"
  def install
    odie "Placeholder formula — wait for the first release."
  end
end
EOF

git add Casks Formula
git commit -m "Seed tap with placeholder cask + formula"
git push origin main
```

**Lesson learned:** Homebrew Cask only accepts `conflicts_with cask:`, **never** `conflicts_with formula:`. That stanza on the cask side fails the cask validator at install time with "Unknown key: :formula". The reciprocal `conflicts_with cask: "contextfs"` on the formula side is sufficient — brew refuses to install the formula whenever the cask is present. `scripts/render-homebrew.py` no longer emits any conflicts_with line in the cask.

### 4.3 Fine-grained PAT for cross-repo push

Visit `https://github.com/settings/personal-access-tokens/new`:

| Field | Value |
|---|---|
| Token name | `ctxfs-homebrew-tap-bump` |
| Expiration | 90 days |
| Resource owner | `Derek-X-Wang` |
| Repository access | **Only select** → `Derek-X-Wang/homebrew-ctxfs` |
| Repository permissions | **Contents**: Read & Write. **Pull requests**: Read & Write. |

Generate, copy the `github_pat_…` token, and set the secret:

```bash
read -s -p "HOMEBREW_TAP_PAT: " PAT && echo
printf '%s' "$PAT" | gh secret set HOMEBREW_TAP_PAT --repo Derek-X-Wang/ctxfs
unset PAT
```

**Lesson learned:** `gh repo clone` uses HTTPS without persisting the token to git's remote URL, so a subsequent `git push` falls back to interactive auth and fails with `fatal: could not read Username for 'https://github.com'`. `publish-metadata.yml` rewrites the remote URL to embed the token after cloning:

```bash
git remote set-url origin "https://x-access-token:${GH_TOKEN}@github.com/${HOMEBREW_TAP_REPO}.git"
```

Calendar reminder: rotate this PAT every 85 days. Symptom of expiry is `publish-metadata.yml` opening a "Tap bump failed for vX.Y.Z" tracking issue on every release — visible, not silent.

---

## 5. `gh-pages` branch — Sparkle appcast hosting

Sparkle reads `https://derek-x-wang.github.io/ctxfs/appcast.xml`. GitHub Pages serves from the `gh-pages` branch.

### 5.1 Create the orphan branch

If recovering from a wipe and `gh-pages` is gone, create it as an orphan branch with seed content. Use a fresh shallow clone in `/tmp` rather than swapping branches in your working tree:

```bash
cd /tmp && rm -rf ctxfs-ghpages-bootstrap
gh repo clone Derek-X-Wang/ctxfs ctxfs-ghpages-bootstrap -- --depth 1
cd ctxfs-ghpages-bootstrap
git checkout --orphan gh-pages
git rm -rf .

cat > appcast.xml <<'EOF'
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

cat > index.html <<'EOF'
<!doctype html>
<html><head>
<meta http-equiv="refresh" content="0; url=https://github.com/Derek-X-Wang/ctxfs">
</head><body>Redirecting to <a href="https://github.com/Derek-X-Wang/ctxfs">github.com/Derek-X-Wang/ctxfs</a>.</body></html>
EOF

git add appcast.xml index.html
git commit -m "chore(gh-pages): seed appcast.xml + index redirect"
git push origin gh-pages -u
```

### 5.2 Enable Pages

```bash
gh api -X POST repos/Derek-X-Wang/ctxfs/pages --input - <<'JSON'
{"source": {"branch": "gh-pages", "path": "/"}}
JSON
```

Already-enabled returns HTTP 409 — fine. Verify after ~90 seconds:

```bash
curl -fsSL https://derek-x-wang.github.io/ctxfs/appcast.xml | head -3
```

---

## 6. Required CI runner facts

Both `.github/workflows/release.yml` and `.github/workflows/ci.yml` depend on platform specifics that aren't obvious from the YAML. Don't change these without re-validating end-to-end.

### release.yml

- **Runner:** `runs-on: macos-26` (Intel variant; arm64 pool had 65+ min queue waits during dress rehearsal). The lipo step still produces a working universal binary because `cargo build --target aarch64-apple-darwin --target x86_64-apple-darwin` cross-compiles both targets from any arch.
- **Xcode:** explicitly `xcode-select -s /Applications/Xcode_26.4.app`. macos-26's default is 26.2, whose macOS 26.2 SDK does not have `FSVolume.MountOptions`. ContextFSExt/Volume.swift uses that type → 26.4 minimum.
- **Brew installs:** `create-dmg`, `protobuf`, `swift-protobuf`. Without `protobuf`, `fskit-rs`'s prost build script fails. Without `swift-protobuf`, the pbxproj `protoc --swift_out` build rule fails to find `protoc-gen-swift`.
- **Sparkle CLI:** downloaded fresh per run, SHA-pinned. Action cache key includes `SPARKLE_VERSION`.
- **No `xcodebuild | tail`:** the pipe masks BUILD FAILED. The workflow uses `set -o pipefail` and emits full xcodebuild output. Don't reintroduce the truncation.

### publish-metadata.yml

- **Runner:** `runs-on: ubuntu-latest`. xmllint is **not** installed by default. Validate appcast XML via stdlib `python3 -c "import xml.etree.ElementTree as ET; ET.parse('…')"`.
- **`gh repo clone` doesn't persist auth.** After clone, rewrite the remote URL: `git remote set-url origin "https://x-access-token:${GH_TOKEN}@github.com/${HOMEBREW_TAP_REPO}.git"`.
- **`scripts/append-appcast-item.py` is idempotent** on `sparkle:shortVersionString`. If two retries fire for the same release tag, the second is a no-op.

### ci.yml

- macos-latest needs `brew install protobuf swift-protobuf`. Linux needs `apt-get install protobuf-compiler` alongside `nfs-common`.
- `ctxfs-fskit/src/adapter.rs` uses `libc::ENOATTR`, which is **deprecated on Linux** (use `ENODATA`). The crate cfg-gates it on `target_os = "macos"` so cross-compilation on the Linux CI runner stays clean.
- `is_macos_26_or_later` in `ctxfs-cli/src/setup.rs` is `#[allow(dead_code)]` — the only callers are macOS-gated, so Linux clippy sees it as dead.

---

## 7. Re-running a release

After all eight systems are in place, the release flow is:

```bash
# 1. Write release notes
$EDITOR .github/release-notes/v0.X.Y.md

# 2. Commit notes
git add .github/release-notes/v0.X.Y.md
git commit -m "docs(release): v0.X.Y notes"

# 3. Stamp + tag
scripts/release.sh 0.X.Y

# 4. Push
git push origin main
git push origin v0.X.Y

# 5. Watch
gh run watch --repo Derek-X-Wang/ctxfs
```

Tag push triggers `release.yml`. ~10–15 min later, a draft GitHub Release exists with 6 artifacts. Inspect, install the DMG locally, verify (`spctl -a -vv`, `codesign --verify --strict`, `ctxfs --version`).

When satisfied, publish:

```bash
gh release edit v0.X.Y --repo Derek-X-Wang/ctxfs --draft=false --latest
```

That triggers `publish-metadata.yml`:
- Appends the version to `gh-pages/appcast.xml`
- Renders new cask + formula
- Opens a PR against `Derek-X-Wang/homebrew-ctxfs`

Merge the tap PR (`gh pr merge --squash --delete-branch`). Sparkle and Homebrew users see the new version within minutes.

---

## 8. Key rotation

### Sparkle EdDSA key (annual or on suspected leak)

Verbatim from the Phase 3 spec (Section 6.8). Sparkle 2.x supports a graceful 2-release rollover.

1. **Generate new keypair:** `./bin/generate_keys --replace` (Sparkle tool writes a new key; old one moves aside in Keychain).
2. **Update `SPARKLE_PRIVATE_KEY` secret in GitHub Actions** with the new key (section 3.2 above).
3. **Embed both public keys in `Info.plist`** for one release cycle:
   - `SUPublicEDKey` = the **new** public key (single string)
   - `SUPublicEDKeys` = an array containing **both** the old and new public keys
   - Sparkle 2.x checks `SUPublicEDKeys` first, falls back to `SUPublicEDKey`, and accepts updates signed by any key in the array.
4. **Release `vX+1.0` signed with the *old* private key** — existing installs trust this update because it's signed by the key they already know. This release's `Info.plist` carries both keys, so installs after this point trust both keys going forward.
5. **Release `vX+2.0` signed with the *new* private key**. Remove the old public key from `Info.plist` (drop `SUPublicEDKeys` back to a single-element array containing only the new key, or remove it entirely and rely on `SUPublicEDKey`).
6. **Done.** Existing installs auto-update through this path without any user action; the rolled-over chain takes two release cycles total.

If you skip the dual-key step (replace `SUPublicEDKey` directly), every existing install becomes orphaned — Sparkle rejects updates because the signature doesn't verify, and there's no in-band recovery. Only do that for a freshly-bootstrapped pipeline with zero installs in the wild.

### Developer ID Application certificate (~5 year cycle)

Standard Apple Developer portal flow:

1. **Apple Developer portal → Certificates** → revoke old cert (or wait for it to expire).
2. **+** → **Developer ID Application** → CSR flow → install new cert in login keychain.
3. **Re-export `.p12`** (section 2.1) and update `DEVELOPER_ID_P12_BASE64` + `DEVELOPER_ID_P12_PASSWORD` secrets.
4. **Provisioning profiles tied to the old cert stop working.** Regenerate both profiles (section 1.3) bound to the new cert. Re-export their `.base64` and update `DEVELOPER_ID_APP_PROFILE_BASE64` + `DEVELOPER_ID_EXT_PROFILE_BASE64`.
5. **Profile-specifier strings in pbxproj don't change** (we kept the same names). No code change required.
6. **Bump `xcode-select` target in `release.yml` if the new cert was issued under a newer Xcode** that ships a new SDK we now require.

Existing notarized installs stay valid forever (notarization tickets are independent of cert lifetime). Only future builds need the new cert.

### `HOMEBREW_TAP_PAT` (90-day cycle)

GitHub fine-grained PATs cap at 90 days. Set a calendar reminder for ~85 days from issuance.

1. Generate a new token at `https://github.com/settings/personal-access-tokens/new` (same scope as section 4.3).
2. `read -s -p "HOMEBREW_TAP_PAT: " PAT && printf '%s' "$PAT" | gh secret set HOMEBREW_TAP_PAT --repo Derek-X-Wang/ctxfs && unset PAT`
3. Revoke the old token at the same URL.

If the reminder is missed, the symptom is visible: `publish-metadata.yml:178` opens an issue titled `Tap bump failed for vX.Y.Z` and re-raises the failure on every release. Appcast regeneration still succeeds — only the Homebrew tap PR fails — so Sparkle keeps working; users just don't see the new version via `brew upgrade`.

---

## 9. Reference: secret inventory

The full set of secrets `release.yml` and `publish-metadata.yml` read. If you add another, mirror the entry here.

| Secret | Source | Workflow | Set in |
|---|---|---|---|
| `APPLE_ID` | Apple ID email | release.yml | §2.3 |
| `APPLE_ID_PASSWORD` | App-specific password | release.yml | §2.3 (gen in §2.2) |
| `APPLE_TEAM_ID` | `RDQSC33B2X` | release.yml | §2.3 |
| `DEVELOPER_ID_P12_BASE64` | Keychain `.p12` export | release.yml | §2.3 (export in §2.1) |
| `DEVELOPER_ID_P12_PASSWORD` | Password for that `.p12` | release.yml | §2.3 |
| `DEVELOPER_ID_APP_PROFILE_BASE64` | `ContextFS_Distribution.provisionprofile` | release.yml | §2.3 |
| `DEVELOPER_ID_EXT_PROFILE_BASE64` | `ContextFS_Extension_Distribution.provisionprofile` | release.yml | §2.3 |
| `SPARKLE_PRIVATE_KEY` | Login-keychain ed25519 entry | release.yml | §3.2 |
| `HOMEBREW_TAP_PAT` | github.com fine-grained PAT (90d) | publish-metadata.yml | §4.3 |

`gh secret list --repo Derek-X-Wang/ctxfs` should always show exactly these nine plus any unrelated entries (dependabot, etc.).

---

## 10. What's *not* in this runbook

These are deliberate scope cuts for Phase 3 — don't introduce them without a separate spec:

- **`cargo install ctxfs` / crates.io publish.** The CLI is shipped via Homebrew formula + GitHub release tarball. Never run `cargo publish` from this repo's CI.
- **Linux binaries.** macOS only.
- **Sparkle delta updates.** Full app downloads are fine for a <50 MB bundle.
- **Curl-pipe-shell installer.** `ctxfs update` covers non-Homebrew users after first install.
- **Telemetry / crash reporting.** Out of scope.
- **Auto-check-for-updates on the CLI.** Sparkle handles GUI; CLI requires explicit `ctxfs update`.

If any of these become needed, write a Phase 3.5+ spec rather than bolting them onto this pipeline.
