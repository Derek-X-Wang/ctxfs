# Phase 3e — Bootstrap + First Release Plan

> **For agentic workers:** Most of this plan is human-only actions (web UI clicks, Keychain access, `defaults` toggles). The agent's role here is coordinator — guide Derek through the runbook, verify each gate, unblock when verification fails. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bootstrap every external system the Phase 3 pipeline depends on (Apple Developer portal, GitHub Actions secrets, Homebrew tap repo, gh-pages branch), then exercise the pipeline end-to-end via a `v0.0.1` dress rehearsal, then cut the real `v0.1.0` soft-launch release.

**Architecture:** Linear runbook, not a parallel task graph. Each stage must complete before the next starts — bootstrap is deeply sequential: signing cert → provisioning profile → workflow secret → CI run → release. Three waiting periods are expected: Apple Developer portal capability propagation (minutes), CI pipeline (~15 min per run), Apple notarization (~2-10 min per artifact).

**Tech Stack:**
- Apple Developer portal (web) + Xcode Keychain Access
- Sparkle's `generate_keys` CLI tool (already downloaded in Phase 3a bootstrap)
- GitHub web UI for repo creation + Actions secret management
- `gh` CLI for the dress-rehearsal tag + release cleanup
- `scripts/release.sh` from Phase 3c

**What's out of scope for 3e** (post-1.0):
- Public announcement (Twitter / Show HN) — happens *after* Derek dogfoods for 1–2 weeks
- A landing page at ctxfs.ai — no custom domain in Phase 3
- Signed shell installer / `curl | sh` — Phase 3.5+
- Any Linux user-facing work

3e's ship criterion: `brew install --cask contextfs` succeeds on a clean macOS machine, Sparkle auto-detects v0.1.0 as the newest release, and `ctxfs update --check` agrees.

---

## File structure

Only one file is created by this plan (the rest is external-system bootstrap):

| File | Responsibility |
|---|---|
| `docs/phase3-bootstrap-runbook.md` | NEW — cleaned-up version of this runbook, committed alongside the first real release so the "how to re-bootstrap if the laptop dies" answer lives in the repo. Written in Stage 9 after we know what actually worked. |

---

## Stage 1: Apple Developer portal App IDs + profiles

This is the irreducibly-web step. If Derek's Apple ID isn't signed into a paid Apple Developer Program membership, stop here — the rest of Phase 3 depends on it.

- [ ] **1.1 Confirm membership**

Visit `https://developer.apple.com/account` — the landing page shows membership status. Required: **active paid Apple Developer Program membership** under team `RDQSC33B2X` (Derek's team).

If status shows **expired** or **not enrolled**, renew first ($99/year). Everything downstream needs a valid team.

- [ ] **1.2 Create App IDs**

Navigate to **Certificates, Identifiers & Profiles** → **Identifiers** → **+** (add).

Create two App IDs:

| Identifier | Description | Capabilities |
|---|---|---|
| `ai.ctxfs.companion` | ContextFS host app | App Groups (optional — skip for Phase 3), Hardened Runtime |
| `ai.ctxfs.companion.fskitext` | ContextFS FSKit extension | FSKit Module, Hardened Runtime |

**Important:** the `ai.ctxfs.companion.fskitext` App ID **must** have the FSKit Module capability. Without it, notarization will reject the bundled extension.

If the portal doesn't show FSKit Module as an available capability, Apple may still be rolling it out region-wise — contact Apple Developer support.

- [ ] **1.3 Verify a Developer ID Application cert already exists**

Still in **Certificates, Identifiers & Profiles** → **Certificates**. Look for an unexpired **Developer ID Application** certificate.

This already exists from Phase 3a work (Derek has used it for dev builds). If none exists, create one: **+** → **Developer ID Application** → follow the CSR flow (Keychain Access → Certificate Assistant → Request a Certificate From a Certificate Authority).

- [ ] **1.4 Create two Developer ID provisioning profiles**

Navigate to **Profiles** → **+**.

**Profile 1:**
- Type: **Developer ID Distribution** (not Development)
- App ID: `ai.ctxfs.companion`
- Certificates: the Developer ID Application cert from step 1.3
- Name: exactly `ContextFS Distribution` (this string is committed to `project.pbxproj` in step 1.6)
- Download the `.provisionprofile` file

**Profile 2:**
- Type: Developer ID Distribution
- App ID: `ai.ctxfs.companion.fskitext`
- Certificates: same cert
- Name: exactly `ContextFS Extension Distribution`
- Download the `.provisionprofile` file

- [ ] **1.5 Double-check the profiles include FSKit Module capability**

For the extension profile (`ContextFS Extension Distribution`), click the name in the portal list and confirm the **Capabilities** section shows **FSKit Module**. If it doesn't, the App ID (step 1.2) didn't have the capability enabled — go back, fix the App ID, regenerate the profile.

- [ ] **1.6 Commit the profile-specifier names to pbxproj**

Back in the repo, edit `swift/ContextFS/ContextFS.xcodeproj/project.pbxproj`. Per-target Release config needs `PROVISIONING_PROFILE_SPECIFIER`. Use Xcode's GUI:

1. Open the project in Xcode
2. Select the `ContextFS` target → Signing & Capabilities → Release config → set **Provisioning Profile** to `ContextFS Distribution`
3. Select the `ContextFSExt` target → same tab → set **Provisioning Profile** to `ContextFS Extension Distribution`
4. Verify: `grep PROVISIONING_PROFILE_SPECIFIER swift/ContextFS/ContextFS.xcodeproj/project.pbxproj` should show both specifier names
5. Commit:
   ```bash
   git add swift/ContextFS/ContextFS.xcodeproj/project.pbxproj
   git commit -m "build(swift): commit Developer ID provisioning profile specifiers

Phase 3e bootstrap: CI needs xcodebuild to resolve the right profile
per target, and 'xcodebuild KEY=value' overrides apply globally (not
per-target). Committing the names to pbxproj is the only supported
way to scope them per target."
   ```

---

## Stage 2: Export certificate + profiles to GitHub Actions secrets

- [ ] **2.1 Export the Developer ID Application cert**

Open **Keychain Access** on Derek's Mac:
1. **My Certificates** tab
2. Find **Developer ID Application: Xinzhe Wang (RDQSC33B2X)** — note: you want the certificate entry, not the private key alone
3. Right-click → **Export** → choose **Personal Information Exchange (.p12)** format → set a strong password (save it; you'll need it in 2.3)
4. Save as `~/Downloads/ctxfs-developer-id.p12`

- [ ] **2.2 Base64-encode the .p12 + profiles**

```bash
cd ~/Downloads
base64 -i ctxfs-developer-id.p12 -o ctxfs-developer-id.p12.base64
# Verify: the file should be non-empty and ASCII
head -c 60 ctxfs-developer-id.p12.base64 && echo "..."
# Paths to the two .provisionprofile files from step 1.4:
base64 -i ContextFS_Distribution.provisionprofile -o contextfs-app-profile.base64
base64 -i ContextFS_Extension_Distribution.provisionprofile -o contextfs-ext-profile.base64
```

If the file names don't match (Apple tends to download with slightly munged names), adjust accordingly.

- [ ] **2.3 Set the GitHub Actions secrets**

Navigate to the repo on GitHub → **Settings** → **Secrets and variables** → **Actions** → **New repository secret**.

Create these exact secret names (copy-paste from the `.base64` files for the large ones):

| Secret name | Value |
|---|---|
| `DEVELOPER_ID_P12_BASE64` | contents of `ctxfs-developer-id.p12.base64` |
| `DEVELOPER_ID_P12_PASSWORD` | the password you chose in step 2.1 |
| `DEVELOPER_ID_APP_PROFILE_BASE64` | contents of `contextfs-app-profile.base64` |
| `DEVELOPER_ID_EXT_PROFILE_BASE64` | contents of `contextfs-ext-profile.base64` |
| `APPLE_ID` | Derek's Apple ID email (the one associated with team `RDQSC33B2X`) |
| `APPLE_ID_PASSWORD` | app-specific password (see 2.4) |
| `APPLE_TEAM_ID` | `RDQSC33B2X` |

- [ ] **2.4 Generate an app-specific password for notarytool**

Apple requires an app-specific password for `notarytool`, separate from Derek's Apple ID login password.

1. Visit `https://appleid.apple.com/account/manage` → **Sign-In and Security** → **App-Specific Passwords** → **Generate Password**
2. Label it `ctxfs-ci` (or similar — just so Derek can find and revoke it later)
3. Copy the generated string — format like `abcd-efgh-ijkl-mnop`
4. Paste into the `APPLE_ID_PASSWORD` secret from step 2.3

- [ ] **2.5 Shred the local .p12 + .base64 files**

```bash
rm -P ~/Downloads/ctxfs-developer-id.p12 \
      ~/Downloads/ctxfs-developer-id.p12.base64 \
      ~/Downloads/contextfs-app-profile.base64 \
      ~/Downloads/contextfs-ext-profile.base64
```

The `.p12` and `.base64` forms both contain the signing private key. The Keychain still has it; these flat files don't need to linger.

The original `.provisionprofile` files (non-base64) can stay in `~/Downloads/` — they're not secret, just config.

---

## Stage 3: Sparkle EdDSA private key → GitHub secret

Phase 3a already generated the keypair; the public key is in `Info.plist`. This stage copies the private key from Keychain to a GitHub Actions secret so CI can sign release zips.

- [ ] **3.1 Verify the keypair still exists in Keychain**

```bash
security find-generic-password -s "https://sparkle-project.org" -a "ed25519" 2>&1 | head -5
```

Expected: prints the generic-password item (password field is censored until you use `-w`). If it says "could not be found," regenerate via `/tmp/sparkle-tools/bin/generate_keys` but **only if you haven't already shipped any Sparkle-signed builds** — regenerating orphans prior installs.

- [ ] **3.2 Extract the private key to a GitHub secret**

```bash
security find-generic-password -s "https://sparkle-project.org" -a "ed25519" -w | \
  gh secret set SPARKLE_PRIVATE_KEY --repo Derek-X-Wang/ctxfs
```

`gh secret set` reads from stdin. This single command moves the key from Keychain to the workflow environment without ever writing it to a file or printing to stdout.

Verify the secret exists:
```bash
gh secret list --repo Derek-X-Wang/ctxfs | grep SPARKLE_PRIVATE_KEY
```

Expected: one line showing `SPARKLE_PRIVATE_KEY` and its creation timestamp.

- [ ] **3.3 Pin the Sparkle tarball SHA-256 in release.yml**

The `release.yml` workflow left `SPARKLE_TARBALL_SHA256: ''` as a placeholder. Phase 3e fills it in now.

```bash
# Recompute from the tarball we downloaded in Phase 3a
shasum -a 256 /tmp/Sparkle-2.7.0.tar.xz | awk '{print $1}'
```

Take that 64-char hex output, open `.github/workflows/release.yml`, find `SPARKLE_TARBALL_SHA256: ''`, replace with `SPARKLE_TARBALL_SHA256: '<the-hex>'`. Commit:

```bash
git add .github/workflows/release.yml
git commit -m "build(ci): pin Sparkle 2.7.0 tarball SHA-256

Phase 3e bootstrap. Stops release.yml from silently skipping the
integrity check on the Sparkle tools download."
```

---

## Stage 4: Homebrew tap repo

The tap is a separate GitHub repo that holds the cask + formula recipes. Job 2 of the CI pipeline opens PRs against it on every release.

- [ ] **4.1 Create the repo**

On GitHub: **+** → **New repository**.
- Owner: `Derek-X-Wang`
- Repo name: exactly `homebrew-ctxfs` (the `homebrew-` prefix is a Homebrew convention — enables `brew tap Derek-X-Wang/ctxfs`)
- Visibility: **Public**
- Init with: **Add a README file** (checked)
- License: MIT or Apache-2.0 (either works; matches the main repo)

- [ ] **4.2 Seed the tap with stub recipes**

Clone locally, add stub files so the first Job 2 PR has something to diff against:

```bash
gh repo clone Derek-X-Wang/homebrew-ctxfs /tmp/homebrew-ctxfs-bootstrap
cd /tmp/homebrew-ctxfs-bootstrap

mkdir -p Casks Formula

cat > Casks/contextfs.rb <<'EOF'
# STUB: populated by Derek-X-Wang/ctxfs publish-metadata.yml on first release.
# Do not edit by hand.
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
# Do not edit by hand.
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

cat > README.md <<'EOF'
# homebrew-ctxfs

Homebrew tap for [ContextFS](https://github.com/Derek-X-Wang/ctxfs).

## Install

**Mac app (recommended):**
```bash
brew install --cask Derek-X-Wang/ctxfs/contextfs
```

**CLI only (headless / CI):**
```bash
brew install Derek-X-Wang/ctxfs/contextfs
```

The cask and formula cannot be installed side-by-side — the cask already
ships the CLI bundled with the app.

Recipes are auto-updated by [the main repo's publish-metadata workflow](https://github.com/Derek-X-Wang/ctxfs/actions/workflows/publish-metadata.yml).
Do not edit by hand.
EOF

git add Casks Formula README.md
git commit -m "Seed tap with placeholder cask + formula"
git push origin main
```

- [ ] **4.3 Create the tap PAT for CI**

Back in the main repo's GitHub Settings → **Personal access tokens** (Settings → Developer settings → Personal access tokens → **Fine-grained tokens**):
- Token name: `ctxfs-homebrew-tap-bump`
- Expiration: **90 days**
- Resource owner: `Derek-X-Wang`
- Repository access: **Only select repositories** → `Derek-X-Wang/homebrew-ctxfs`
- Repository permissions:
  - **Contents**: Read and write
  - **Pull requests**: Read and write

Generate. Copy the token value (it's only shown once).

- [ ] **4.4 Set `HOMEBREW_TAP_PAT` secret**

```bash
echo -n "<paste-token-here>" | gh secret set HOMEBREW_TAP_PAT --repo Derek-X-Wang/ctxfs
```

Verify:
```bash
gh secret list --repo Derek-X-Wang/ctxfs | grep HOMEBREW_TAP_PAT
```

- [ ] **4.5 Calendar reminder for PAT rotation**

Token expires in 90 days. Create a calendar event for 85 days from now: "Rotate ctxfs-homebrew-tap-bump PAT." Miss this and Job 2 silently fails on every release until you rotate.

---

## Stage 5: gh-pages branch for appcast hosting

Sparkle needs `https://derek-x-wang.github.io/ctxfs/appcast.xml` to respond. GitHub Pages serves from the `gh-pages` branch.

- [ ] **5.1 Create the orphan `gh-pages` branch**

From the main repo clone:

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs

# Save current branch so we come back
ORIGINAL_BRANCH=$(git rev-parse --abbrev-ref HEAD)

# Create orphan branch (no history from main)
git checkout --orphan gh-pages
git rm -rf .
```

- [ ] **5.2 Seed the branch with an empty appcast + placeholder index**

```bash
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
<html>
<head>
  <title>ContextFS</title>
  <meta http-equiv="refresh" content="0; url=https://github.com/Derek-X-Wang/ctxfs">
</head>
<body>
  <p>Redirecting to <a href="https://github.com/Derek-X-Wang/ctxfs">github.com/Derek-X-Wang/ctxfs</a>.</p>
</body>
</html>
EOF

git add appcast.xml index.html
git commit -m "chore(gh-pages): seed appcast.xml + index redirect

Phase 3e bootstrap. CI Job 2 (publish-metadata.yml) appends items
to appcast.xml on every published release.

index.html redirects browsers to the main repo — nothing else
should be served from this branch."

git push origin gh-pages -u
git checkout "$ORIGINAL_BRANCH"
```

- [ ] **5.3 Enable GitHub Pages serving the branch**

On GitHub → repo → **Settings** → **Pages**:
- **Source**: `Deploy from a branch`
- **Branch**: `gh-pages` / `/ (root)`
- Click **Save**

First deploy takes ~1 minute. Verify:
```bash
sleep 90
curl -fsSL https://derek-x-wang.github.io/ctxfs/appcast.xml | head -3
```

Expected: first three lines of the XML you just committed. If you get a 404, wait another minute and retry — GitHub Pages is sometimes slow on first deploy.

---

## Stage 6: `v0.0.1` dress rehearsal

Now everything's bootstrapped. Push a throwaway tag to exercise the full pipeline end-to-end.

- [ ] **6.1 Write the v0.0.1 release notes**

Phase 3d already landed `.github/release-notes/v0.0.1.md` as a stub. Good — no edit needed.

- [ ] **6.2 Run the release script for v0.0.1**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
scripts/release.sh 0.0.1
```

Expected output: `==> Releasing v0.0.1 ... chore: release v0.0.1 ... Done.`

Review the commit:
```bash
git show HEAD --stat
git log -1 --format=%B
```

- [ ] **6.3 Push the tag + commit**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
git push origin main
git push origin v0.0.1
```

The tag push triggers `release.yml`. Watch it:
```bash
gh run watch --repo Derek-X-Wang/ctxfs
```

Or open the Actions tab in the browser.

Expected duration: ~10-15 minutes. If it exceeds 30 minutes, something stuck on notarization — inspect the step output.

- [ ] **6.4 Pipeline failure triage (if needed)**

If `release.yml` fails, common causes:

| Failure point | Likely cause | Fix |
|---|---|---|
| "Import Developer ID cert" | `DEVELOPER_ID_P12_BASE64` not set or corrupted | Re-run Stage 2.2 + 2.3 |
| "Install provisioning profiles" | Profile name or App ID mismatch | Stage 1.4 — regenerate profile |
| "xcodebuild" | `PROVISIONING_PROFILE_SPECIFIER` in pbxproj doesn't match profile name | Stage 1.6 — fix pbxproj |
| "Notarize" | App-specific password wrong or `APPLE_ID` wrong | Stage 2.4 — regenerate app-specific password |
| "sign_update" | `SPARKLE_PRIVATE_KEY` wrong | Stage 3.2 — re-extract from Keychain |
| "gh release create" | Release notes file not committed | Check `.github/release-notes/v0.0.1.md` exists in the tagged commit |

If you need to re-try: delete the failed tag both locally and remotely, fix the issue, re-cut with `scripts/release.sh`:

```bash
git tag -d v0.0.1
git push --delete origin v0.0.1
gh release delete v0.0.1 --yes --repo Derek-X-Wang/ctxfs 2>/dev/null || true
git reset --hard HEAD~1  # undo the release commit
# Now fix the bootstrap issue, then re-run scripts/release.sh 0.0.1
```

- [ ] **6.5 Download the draft + inspect**

When `release.yml` succeeds, the draft Release is visible at `https://github.com/Derek-X-Wang/ctxfs/releases`. Download the DMG:

```bash
gh release download v0.0.1 --pattern 'ContextFS-0.0.1.dmg' \
  --dir /tmp/ctxfs-rc-test --repo Derek-X-Wang/ctxfs
open /tmp/ctxfs-rc-test/ContextFS-0.0.1.dmg
```

The DMG should mount without Gatekeeper complaints. Drag `ContextFS.app` to `/Applications/`.

- [ ] **6.6 Sanity-check the installed .app**

```bash
# Verify signature + notarization
spctl -a -vv /Applications/ContextFS.app
codesign --verify --strict --verbose=4 /Applications/ContextFS.app

# Launch it
open /Applications/ContextFS.app
```

Expected: Gatekeeper accepts, app launches, menu bar icon appears, no console errors.

If the app crashes or Gatekeeper rejects it, the signing/notarization pipeline produced a broken artifact. Do **not** publish — go back to 6.4 triage.

- [ ] **6.7 Discard the v0.0.1 draft release**

```bash
gh release delete v0.0.1 --yes --repo Derek-X-Wang/ctxfs
git push --delete origin v0.0.1
git tag -d v0.0.1
git reset --hard HEAD~1  # undo the release commit
git push --force-with-lease origin main
```

Clean slate. `v0.0.1` never reached users.

---

## Stage 7: Address any issues discovered

- [ ] **7.1 If Stage 6 surfaced pipeline bugs, fix them now**

Common post-dress-rehearsal polish items we might discover:
- Hardened runtime entitlement missing on a nested framework
- DMG background image missing (`swift/ContextFS/resources/dmg-bg.png` doesn't exist — the create-dmg flag referencing it fails loudly in that case)
- Sparkle public key in the bundled .app doesn't match the private key CI signed with (mismatched generate_keys runs)

If you fix anything, commit and re-do Stage 6 against a fresh `v0.0.1`.

- [ ] **7.2 Tighten the workflow if Stage 6 was very slow**

The /simplify pass already applied cargo parallelism + rust-cache + parallel notarization. If the run still took > 20 minutes, profile the slow steps via the workflow logs — unusual times suggest Apple notary service was under load (just re-run) or a cold `target/` cache (one-time; warm run will be faster).

---

## Stage 8: `v0.1.0` real release

The dress rehearsal passed. Cut the real first release.

- [ ] **8.1 Write real v0.1.0 release notes**

`.github/release-notes/v0.1.0.md` currently has the Phase 3c template. Open it, flesh it out:
- Replace the "First public release..." paragraph with a real description
- Update "What's in the box" with concrete feature list
- Review the "Known limitations" list for current accuracy
- Save + commit

- [ ] **8.2 Run the release script**

```bash
scripts/release.sh 0.1.0
git log -1 --stat
```

- [ ] **8.3 Push + watch**

```bash
git push origin main
git push origin v0.1.0
gh run watch --repo Derek-X-Wang/ctxfs
```

Same duration as the dress rehearsal (~15 min).

- [ ] **8.4 Install draft on a second Mac**

This is the ultimate test — a Mac that's never run the dev cert, never touched our Keychain. Borrow one. Or use a second machine you own.

```bash
# On the second Mac:
gh release download v0.1.0 --pattern 'ContextFS-0.1.0.dmg' \
  --dir /tmp/ctxfs-v0.1.0 --repo Derek-X-Wang/ctxfs
open /tmp/ctxfs-v0.1.0/ContextFS-0.1.0.dmg
# Drag to /Applications/
# Open /Applications/ContextFS.app
# Go through the onboarding wizard
# Mount github:octocat/Hello-World @ master
# Unmount cleanly
# Quit the app
```

Any of the following failing is a publish-blocker:
- Gatekeeper rejects the DMG (even with the new notarization)
- Onboarding wizard crashes
- Mount + read doesn't work
- FSKit extension isn't registered after toggling it in System Settings

- [ ] **8.5 Publish the draft**

On GitHub: `https://github.com/Derek-X-Wang/ctxfs/releases` → click `v0.1.0` → click **Edit release** → uncheck "Set as a pre-release" if it's set → click **Publish release**.

This triggers `publish-metadata.yml`. Watch it:
```bash
gh run watch --repo Derek-X-Wang/ctxfs
```

Expected:
- `gh-pages` branch gets a new commit adding the v0.1.0 `<item>` to appcast.xml
- `Derek-X-Wang/homebrew-ctxfs` gets a new PR titled `Bump contextfs to v0.1.0`

- [ ] **8.6 Merge the Homebrew tap PR**

Review the PR, verify the SHA-256s match, merge.

```bash
gh pr list --repo Derek-X-Wang/homebrew-ctxfs
# Copy the PR number, then:
gh pr merge <number> --repo Derek-X-Wang/homebrew-ctxfs --squash
```

- [ ] **8.7 Verify end-to-end from a clean Mac**

```bash
# On any Mac (the second one from 8.4 is ideal):
brew untap Derek-X-Wang/ctxfs 2>/dev/null || true
brew tap Derek-X-Wang/ctxfs
brew install --cask contextfs
# Verify app installed:
ls /Applications/ContextFS.app
# Verify bundled CLI is on PATH:
which ctxfs
ctxfs --version
```

Expected: `ctxfs --version` prints `0.1.0` (or whatever VERSION file says).

- [ ] **8.8 Verify Sparkle sees the release**

On the same Mac:
```bash
defaults delete ai.ctxfs.companion SUFeedURL 2>/dev/null || true  # remove any dev override
open /Applications/ContextFS.app
# Click menu icon → Check for Updates…
```

Expected: Sparkle says "You're up to date" (the installed version matches the latest appcast item). This proves the appcast is live + the EdDSA signature matches.

---

## Stage 9: Document + dogfood

- [ ] **9.1 Write the bootstrap runbook**

Create `docs/phase3-bootstrap-runbook.md` by copying the stages from this plan, stripping the agent-directed checklist framing, and adding any fixes discovered during 3e. This lives in the repo as the authoritative "how to re-bootstrap if the laptop dies" doc.

Commit + push.

- [ ] **9.2 Private dogfood for 1–2 weeks**

Use ctxfs daily. Track:
- Any panics / crashes in the daemon
- Any Sparkle update dialog glitches
- Any mount failures under real load
- CI pipeline time / cost / failures

File GitHub issues for each. No public announcement yet.

- [ ] **9.3 First patch release (if needed)**

If dogfooding surfaces issues, cut `v0.1.1` the same way (`scripts/release.sh 0.1.1` after fixes + release-notes file). The full pipeline is now proven — every subsequent release is `git push --tags` + wait.

---

## Stage 10: Public soft launch (~2 weeks after v0.1.0)

- [ ] **10.1 Confirm no blockers from dogfooding**

Review the issue tracker. Nothing P0 open. Pipeline reliability stable across ≥3 releases.

- [ ] **10.2 Post on Twitter / Show HN**

Short post linking to `https://github.com/Derek-X-Wang/ctxfs`. Include:
- One-sentence pitch ("Mount any Git repo without cloning")
- Install command: `brew install --cask contextfs`
- Screenshot of the menu bar app with a mounted repo

- [ ] **10.3 Monitor feedback for 48 hours**

Be responsive to issues in the first 48 hours. This is when early users trip over first-install papercuts.

- [ ] **10.4 Celebrate**

Derek shipped a signed, notarized, auto-updating Mac app + CLI with a Homebrew tap. That's a real milestone.

---

## Self-review checklist

**Spec coverage:** Plan 3e covers spec Section 6 (One-time bootstrap) fully: Apple Developer portal setup, secret export scripts (embedded as inline commands here rather than a separate `scripts/bootstrap-secrets.sh` since the one-shot nature doesn't warrant a committed script), Sparkle key generation + extraction, Homebrew tap seed, gh-pages bootstrap, dress rehearsal, first release, key rotation runbook (deferred — noted in the 90-day calendar reminder).

**Placeholder scan:** No "TBD" or "fill in later." The only intentionally-placeholder content is the stub cask/formula in Stage 4.2, which is correct behavior (they're meant to be overwritten by the first Job 2 PR).

**Type consistency:** Tag format (`vX.Y.Z`) vs version format (`X.Y.Z`) is consistent — `scripts/release.sh` bridges them, CI workflows derive both from `GITHUB_REF_NAME`. App bundle IDs (`ai.ctxfs.companion` / `ai.ctxfs.companion.fskitext`) match across Apple Developer portal, pbxproj, CI workflow env vars.

**Known edges the plan does NOT solve** (by design):
- Key rotation under compromise (referenced in spec Section 6.8 — runbook lives in the spec, not here; copy into `docs/phase3-bootstrap-runbook.md` in Stage 9.1 if you want it in the repo).
- PAT rotation automation — 90-day expiry is a calendar reminder, not a CI reminder.
- Multi-maintainer signing — only Derek can sign/notarize. If this project grows, move to a CI-only signing workload + passkey or hardware key.
