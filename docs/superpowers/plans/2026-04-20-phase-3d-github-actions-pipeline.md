# Phase 3d — GitHub Actions Release Pipeline Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two GitHub Actions workflows that take a `vX.Y.Z` tag push through full build + Developer ID signing + notarization + artifact upload (Job 1), then on manual release publish regenerate the Sparkle appcast and open a Homebrew tap bump PR (Job 2). Plus two Python helper scripts (`render-homebrew.py`, `append-appcast-item.py`) invoked from the workflows so the XML/Ruby generation is deterministic and unit-testable.

**Architecture:** Two separate workflow files. `release.yml` runs on `push: tags: 'v*.*.*'`, takes ~10-15 min on macos-14, produces a **draft** Release with all artifacts attached. Derek manually inspects the draft + installs on a second Mac, then clicks "Publish release." That triggers `publish-metadata.yml` which regenerates the appcast on the `gh-pages` branch and opens a tap-bump PR on `Derek-X-Wang/homebrew-ctxfs`. Both workflows use explicit `permissions:` blocks scoped to the minimum (`contents: write` + `issues: write` on Job 2).

**Tech Stack:**
- GitHub Actions with `macos-14` runners for Job 1, `ubuntu-latest` for Job 2
- Xcode 17.x (bundled), `notarytool` + `stapler` + `codesign` + `lipo` (Xcode-bundled)
- `create-dmg` via Homebrew (version-pinned)
- Sparkle 2.7.x CLI tools (downloaded + SHA-256-verified in the workflow)
- Python 3 for the two helper scripts — stdlib only, no PyPI deps (keeps dev setup trivial)
- `gh` CLI for Release asset manipulation

**What's out of scope for 3d** (belongs to Phase 3e):
- Actually pushing a `vX.Y.Z` tag (3e dress rehearsal with `v0.0.1-rc1` first, then real `v0.1.0`)
- Bootstrap secret setup (Apple Developer portal, keychain export, Sparkle key generation, tap repo creation, gh-pages seed) — 3e handles these; 3d just documents which secrets the workflows read
- Signing the workflows themselves or enforcing attestation — out of scope for a soft launch

3d's ship criterion: both workflow YAMLs parse (`actionlint` clean), both helper scripts have passing unit tests, and Derek can visually inspect every step in `release.yml` and `publish-metadata.yml` and trace them back to the spec's Section 3. Full end-to-end CI run happens in Phase 3e.

---

## File structure

Files created or modified by this plan:

| File | Responsibility |
|---|---|
| `.github/workflows/release.yml` | NEW — Job 1 build-and-sign on `push: tags: 'v*.*.*'`. |
| `.github/workflows/publish-metadata.yml` | NEW — Job 2 publish-metadata on `release: published` or `workflow_dispatch`. |
| `scripts/render-homebrew.py` | NEW — renders Casks/contextfs.rb + Formula/contextfs.rb from version + URLs + SHA-256s. |
| `scripts/append-appcast-item.py` | NEW — prepends a new `<item>` to `appcast.xml` with proper XML escaping. |
| `.github/release-notes/v0.0.1-rc1.md` | NEW — stub notes for the Phase 3e dress-rehearsal tag. |
| `tests/scripts/test_render_homebrew.py` | NEW — unit tests for the Python helpers. |
| `tests/scripts/test_append_appcast_item.py` | NEW — ditto. |

---

## Task 1: `.github/workflows/release.yml` — Job 1 build-and-sign

**Files:**
- Create: `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/.github/workflows/release.yml`

This is the heart of the pipeline. ~180 lines of YAML implementing spec Section 3 Job 1 steps 1–20.

- [ ] **Step 1: Create the workflow file**

Write `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/.github/workflows/release.yml`:

```yaml
name: Release

# Tag-driven release pipeline. Pushing a `vX.Y.Z` tag triggers the build,
# sign, notarize, and draft-Release sequence. Job 2 (publish-metadata.yml)
# fires after Derek manually publishes the draft.
on:
  push:
    tags:
      - 'v*.*.*'

# Principle of least privilege: this workflow writes to this repo's Releases
# and nothing else. No cross-repo, no issues, no pull-requests.
permissions:
  contents: write

# Single concurrency group per tag — if someone pushes a tag twice by
# accident, the second run cancels the first.
concurrency:
  group: release-${{ github.ref_name }}
  cancel-in-progress: false   # Never cancel in-progress signing; just queue.

env:
  # Pinned tools. Bumping requires a PR that also updates the cache key.
  XCODE_APP_PATH: /Applications/Xcode_17.app
  SPARKLE_VERSION: '2.7.0'
  SPARKLE_TARBALL_SHA256: ''   # Populated on first Phase 3e run; see README.

jobs:
  build-and-sign:
    name: Build, sign, notarize, draft Release
    runs-on: macos-14
    timeout-minutes: 60

    steps:
      # -------- Checkout + toolchain ------------------------------------

      - name: Checkout
        uses: actions/checkout@v4
        with:
          fetch-depth: 0    # scripts/release.sh used `git rev-list --count HEAD` for build number

      - name: Select Xcode 17
        run: |
          if [ -d "$XCODE_APP_PATH" ]; then
            sudo xcode-select -s "$XCODE_APP_PATH"
          else
            echo "::warning::$XCODE_APP_PATH not found on runner; falling back to default"
          fi
          xcodebuild -version

      - name: Install Rust toolchain
        uses: dtolnay/rust-toolchain@stable
        with:
          targets: aarch64-apple-darwin,x86_64-apple-darwin

      - name: Install create-dmg via Homebrew
        run: |
          brew install create-dmg
          create-dmg --version

      - name: Cache + install Sparkle CLI tools
        id: sparkle-tools
        uses: actions/cache@v4
        with:
          path: /tmp/sparkle-tools
          key: sparkle-${{ env.SPARKLE_VERSION }}

      - name: Download Sparkle CLI tools (cache miss)
        if: steps.sparkle-tools.outputs.cache-hit != 'true'
        run: |
          curl -fL -o /tmp/Sparkle.tar.xz \
            "https://github.com/sparkle-project/Sparkle/releases/download/${SPARKLE_VERSION}/Sparkle-${SPARKLE_VERSION}.tar.xz"
          # Verify SHA-256 if a value is configured (3e bootstrap will set it)
          if [ -n "$SPARKLE_TARBALL_SHA256" ]; then
            echo "${SPARKLE_TARBALL_SHA256}  /tmp/Sparkle.tar.xz" | shasum -a 256 --check
          else
            echo "::warning::SPARKLE_TARBALL_SHA256 not set; skipping integrity check"
          fi
          mkdir -p /tmp/sparkle-tools
          tar -xJf /tmp/Sparkle.tar.xz -C /tmp/sparkle-tools

      # -------- Version assertion --------------------------------------

      - name: Read VERSION + assert match with tag
        id: version
        run: |
          VERSION="$(cat VERSION | tr -d '[:space:]')"
          TAG="${GITHUB_REF_NAME}"
          if [ "v$VERSION" != "$TAG" ]; then
            echo "::error::VERSION file says '$VERSION' but tag is '$TAG' (expected 'v$VERSION')"
            exit 1
          fi
          echo "version=$VERSION" >> "$GITHUB_OUTPUT"
          echo "tag=$TAG" >> "$GITHUB_OUTPUT"

      - name: Assert release notes exist
        run: |
          NOTES=".github/release-notes/${GITHUB_REF_NAME}.md"
          if [ ! -s "$NOTES" ]; then
            echo "::error::$NOTES is missing or empty"
            exit 1
          fi

      # -------- Signing keychain + profiles ---------------------------

      - name: Import Developer ID cert into temp keychain
        env:
          DEVELOPER_ID_P12_BASE64: ${{ secrets.DEVELOPER_ID_P12_BASE64 }}
          DEVELOPER_ID_P12_PASSWORD: ${{ secrets.DEVELOPER_ID_P12_PASSWORD }}
        run: |
          KEYCHAIN_PASSWORD="$(openssl rand -hex 16)"
          echo "KEYCHAIN_PASSWORD=$KEYCHAIN_PASSWORD" >> "$GITHUB_ENV"
          CERT_PATH="$RUNNER_TEMP/cert.p12"

          echo "$DEVELOPER_ID_P12_BASE64" | base64 -D -o "$CERT_PATH"

          security create-keychain -p "$KEYCHAIN_PASSWORD" build.keychain
          security default-keychain -s build.keychain
          security unlock-keychain -p "$KEYCHAIN_PASSWORD" build.keychain
          security set-keychain-settings -lut 7200 build.keychain
          security import "$CERT_PATH" \
            -k build.keychain \
            -P "$DEVELOPER_ID_P12_PASSWORD" \
            -T /usr/bin/codesign \
            -T /usr/bin/productbuild
          security set-key-partition-list -S apple-tool:,apple: -s -k "$KEYCHAIN_PASSWORD" build.keychain

          rm -f "$CERT_PATH"
          security find-identity -p codesigning -v build.keychain

      - name: Install provisioning profiles
        env:
          DEVELOPER_ID_APP_PROFILE_BASE64: ${{ secrets.DEVELOPER_ID_APP_PROFILE_BASE64 }}
          DEVELOPER_ID_EXT_PROFILE_BASE64: ${{ secrets.DEVELOPER_ID_EXT_PROFILE_BASE64 }}
        run: |
          PROFILE_DIR="$HOME/Library/MobileDevice/Provisioning Profiles"
          mkdir -p "$PROFILE_DIR"
          echo "$DEVELOPER_ID_APP_PROFILE_BASE64" | base64 -D -o "$PROFILE_DIR/contextfs_app.provisionprofile"
          echo "$DEVELOPER_ID_EXT_PROFILE_BASE64" | base64 -D -o "$PROFILE_DIR/contextfs_ext.provisionprofile"

      # -------- Rust universal build ----------------------------------

      - name: Build Rust workspace (universal)
        run: |
          cargo build --release --target aarch64-apple-darwin -p ctxfs -p ctxfs-app-helper
          cargo build --release --target x86_64-apple-darwin -p ctxfs -p ctxfs-app-helper
          mkdir -p /tmp/universal
          lipo -create -output /tmp/universal/ctxfs \
            target/aarch64-apple-darwin/release/ctxfs \
            target/x86_64-apple-darwin/release/ctxfs
          lipo -create -output /tmp/universal/ctxfs-app-helper \
            target/aarch64-apple-darwin/release/ctxfs-app-helper \
            target/x86_64-apple-darwin/release/ctxfs-app-helper
          file /tmp/universal/ctxfs   # verify: Mach-O universal binary with 2 architectures

      # -------- Xcode build + nested signing --------------------------

      - name: Build ContextFS.app via xcodebuild (Developer ID)
        env:
          CTXFS_PREBUILT_RUST_DIR: /tmp/universal   # build-rust.sh honors this
        run: |
          xcodebuild \
            -project swift/ContextFS/ContextFS.xcodeproj \
            -scheme ContextFS \
            -configuration Release \
            -derivedDataPath /tmp/ctxfs-build \
            CODE_SIGN_STYLE=Manual \
            DEVELOPMENT_TEAM=RDQSC33B2X \
            CODE_SIGN_IDENTITY="Developer ID Application: Xinzhe Wang (RDQSC33B2X)" \
            OTHER_CODE_SIGN_FLAGS="--options runtime --timestamp" \
            2>&1 | tail -20
          echo "--- built product ---"
          ls /tmp/ctxfs-build/Build/Products/Release/ContextFS.app/Contents/

      - name: Re-sign nested binaries (defense-in-depth)
        run: |
          APP=/tmp/ctxfs-build/Build/Products/Release/ContextFS.app
          for bin in "$APP/Contents/MacOS/ctxfs" "$APP/Contents/MacOS/ctxfs-app-helper"; do
            if [ -f "$bin" ]; then
              codesign --force --sign "Developer ID Application: Xinzhe Wang (RDQSC33B2X)" \
                --options runtime --timestamp "$bin"
            fi
          done
          codesign --force --sign "Developer ID Application: Xinzhe Wang (RDQSC33B2X)" \
            --options runtime --timestamp "$APP"
          codesign --verify --strict --verbose=4 "$APP"

      # -------- Notarize the app ---------------------------------------

      - name: Notarize + staple ContextFS.app
        env:
          APPLE_ID: ${{ secrets.APPLE_ID }}
          APPLE_ID_PASSWORD: ${{ secrets.APPLE_ID_PASSWORD }}
          APPLE_TEAM_ID: ${{ secrets.APPLE_TEAM_ID }}
        run: |
          APP=/tmp/ctxfs-build/Build/Products/Release/ContextFS.app
          ditto -c -k --sequesterRsrc --keepParent "$APP" /tmp/_notary_app.zip
          xcrun notarytool submit /tmp/_notary_app.zip \
            --wait --timeout 30m \
            --apple-id "$APPLE_ID" \
            --password "$APPLE_ID_PASSWORD" \
            --team-id "$APPLE_TEAM_ID"
          xcrun stapler staple "$APP"
          xcrun stapler validate "$APP"
          rm -f /tmp/_notary_app.zip

      # -------- Build DMG + notarize DMG ------------------------------

      - name: Build DMG
        run: |
          VERSION="${{ steps.version.outputs.version }}"
          APP=/tmp/ctxfs-build/Build/Products/Release/ContextFS.app
          mkdir -p /tmp/dmg-artifacts
          create-dmg \
            --volname "ContextFS ${VERSION}" \
            --window-size 500 300 \
            --icon "ContextFS.app" 125 150 \
            --app-drop-link 375 150 \
            "/tmp/dmg-artifacts/ContextFS-${VERSION}.dmg" \
            "$APP"
          ls -la /tmp/dmg-artifacts/

      - name: Sign + notarize + staple DMG
        env:
          APPLE_ID: ${{ secrets.APPLE_ID }}
          APPLE_ID_PASSWORD: ${{ secrets.APPLE_ID_PASSWORD }}
          APPLE_TEAM_ID: ${{ secrets.APPLE_TEAM_ID }}
        run: |
          VERSION="${{ steps.version.outputs.version }}"
          DMG=/tmp/dmg-artifacts/ContextFS-${VERSION}.dmg
          codesign --force --sign "Developer ID Application: Xinzhe Wang (RDQSC33B2X)" \
            --options runtime --timestamp "$DMG"
          xcrun notarytool submit "$DMG" \
            --wait --timeout 30m \
            --apple-id "$APPLE_ID" \
            --password "$APPLE_ID_PASSWORD" \
            --team-id "$APPLE_TEAM_ID"
          xcrun stapler staple "$DMG"

      # -------- Sparkle update archive --------------------------------

      - name: Create Sparkle update zip
        run: |
          VERSION="${{ steps.version.outputs.version }}"
          APP=/tmp/ctxfs-build/Build/Products/Release/ContextFS.app
          ditto -c -k --sequesterRsrc --keepParent "$APP" "/tmp/dmg-artifacts/ContextFS-${VERSION}.zip"

      - name: Sign Sparkle zip (EdDSA)
        env:
          SPARKLE_PRIVATE_KEY: ${{ secrets.SPARKLE_PRIVATE_KEY }}
        run: |
          VERSION="${{ steps.version.outputs.version }}"
          ZIP="/tmp/dmg-artifacts/ContextFS-${VERSION}.zip"
          KEYFILE="$RUNNER_TEMP/sparkle_priv.b64"
          echo "$SPARKLE_PRIVATE_KEY" > "$KEYFILE"
          SIG=$(/tmp/sparkle-tools/bin/sign_update "$ZIP" --ed-key-file "$KEYFILE")
          echo "$SIG" > "/tmp/dmg-artifacts/ContextFS-${VERSION}.zip.sig"
          rm -f "$KEYFILE"
          # Verify round-trip
          /tmp/sparkle-tools/bin/sign_update --verify "$SIG" "$ZIP"

      # -------- CLI tarballs ------------------------------------------

      - name: Sign + package CLI tarballs
        run: |
          VERSION="${{ steps.version.outputs.version }}"
          for arch in arm64 x86_64; do
            if [ "$arch" = "arm64" ]; then
              triple=aarch64-apple-darwin
            else
              triple=x86_64-apple-darwin
            fi
            BIN="target/${triple}/release/ctxfs"
            codesign --force --sign "Developer ID Application: Xinzhe Wang (RDQSC33B2X)" \
              --options runtime --timestamp "$BIN"
            STAGE="$RUNNER_TEMP/cli-${arch}"
            mkdir -p "$STAGE"
            cp "$BIN" "$STAGE/ctxfs"
            cp LICENSE "$STAGE/" 2>/dev/null || true
            cp README.md "$STAGE/" 2>/dev/null || true
            tar -czf "/tmp/dmg-artifacts/ctxfs-${VERSION}-darwin-${arch}.tar.gz" -C "$STAGE" .
          done

      - name: Generate checksums.txt
        run: |
          cd /tmp/dmg-artifacts
          shasum -a 256 *.tar.gz *.dmg *.zip > checksums.txt
          cat checksums.txt

      # -------- Pre-release validation --------------------------------

      - name: Pre-release validation
        run: |
          VERSION="${{ steps.version.outputs.version }}"
          APP=/tmp/ctxfs-build/Build/Products/Release/ContextFS.app
          DMG=/tmp/dmg-artifacts/ContextFS-${VERSION}.dmg

          echo "--- 1. cargo clippy ---"
          cargo clippy --all-targets --tests -- -D warnings

          echo "--- 2. cargo test (nfs backend) ---"
          CTXFS_BACKEND=nfs cargo test --workspace -- --test-threads=1

          echo "--- 3. --version matches VERSION ---"
          "target/aarch64-apple-darwin/release/ctxfs" --version | grep -F "$VERSION"
          "target/x86_64-apple-darwin/release/ctxfs" --version | grep -F "$VERSION"

          echo "--- 4. spctl accepts .app ---"
          spctl -a -vv "$APP"

          echo "--- 5. spctl accepts DMG ---"
          spctl -a -vv --type install "$DMG"

          echo "--- 6. codesign --verify clean ---"
          codesign --verify --strict --verbose=4 "$APP"

          echo "--- 7. every nested binary has hardened runtime + Developer ID ---"
          for bin in "$APP/Contents/MacOS/ctxfs" "$APP/Contents/MacOS/ctxfs-app-helper"; do
            if [ -f "$bin" ]; then
              codesign -dvvv "$bin" 2>&1 | grep -E "Authority=Developer ID|runtime"
            fi
          done

          echo "--- 8. Sparkle sig round-trip already verified in Sign step ---"

          echo "--- 9. unzipped .app runs --version ---"
          UNZIP_DIR=$(mktemp -d)
          ditto -xk "/tmp/dmg-artifacts/ContextFS-${VERSION}.zip" "$UNZIP_DIR"
          "$UNZIP_DIR/ContextFS.app/Contents/MacOS/ctxfs" --version | grep -F "$VERSION"

      # -------- Draft Release -----------------------------------------

      - name: Create draft GitHub Release
        env:
          GH_TOKEN: ${{ github.token }}
        run: |
          VERSION="${{ steps.version.outputs.version }}"
          TAG="${{ steps.version.outputs.tag }}"
          NOTES=".github/release-notes/${TAG}.md"
          cd /tmp/dmg-artifacts
          gh release create "$TAG" \
            --draft \
            --title "$TAG" \
            --notes-file "${GITHUB_WORKSPACE}/${NOTES}" \
            "ContextFS-${VERSION}.dmg" \
            "ContextFS-${VERSION}.zip" \
            "ContextFS-${VERSION}.zip.sig" \
            "ctxfs-${VERSION}-darwin-arm64.tar.gz" \
            "ctxfs-${VERSION}-darwin-x86_64.tar.gz" \
            "checksums.txt"
          echo "Draft release created: ${GITHUB_SERVER_URL}/${GITHUB_REPOSITORY}/releases/tag/${TAG}"

      # -------- Cleanup (runs even on failure) ------------------------

      - name: Clean up keychain
        if: always()
        run: |
          security delete-keychain build.keychain || true
```

- [ ] **Step 2: Validate YAML syntax with actionlint (installed ad-hoc)**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
brew list actionlint >/dev/null 2>&1 || brew install actionlint
actionlint .github/workflows/release.yml
```

Expected: no output (actionlint is silent on clean files). If shellcheck complaints appear, address them — the workflow's `run:` blocks are shell and subject to shellcheck by default.

- [ ] **Step 3: Commit**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
git add .github/workflows/release.yml
git commit -m "feat(ci): release.yml — Job 1 build+sign+notarize+draft Release

Tag-driven release workflow implementing Phase 3 spec Section 3
Job 1. Triggers on 'v*.*.*' tag push. Runs on macos-14 with Xcode
17.x. Timeout 60 min; normal completion ~15 min.

Steps: toolchain setup, version/notes assertions, Developer ID
cert + profile import to temp keychain, universal Rust build,
xcodebuild with manual code-sign style, nested-binary re-sign,
notarize .app, build + sign + notarize DMG, Sparkle zip + EdDSA
sidecar, CLI tarballs (arm64 + x86_64) + checksums.txt,
pre-release validation (clippy, tests, spctl, codesign --verify,
unzip-and-run-version), draft Release with all artifacts.

Secrets read: DEVELOPER_ID_P12_BASE64, DEVELOPER_ID_P12_PASSWORD,
DEVELOPER_ID_APP_PROFILE_BASE64, DEVELOPER_ID_EXT_PROFILE_BASE64,
APPLE_ID, APPLE_ID_PASSWORD, APPLE_TEAM_ID, SPARKLE_PRIVATE_KEY.
Populated in Phase 3e bootstrap.

Uses GITHUB_TOKEN (auto) scoped to contents:write only. Does not
touch cross-repos; Job 2 handles the tap PR."
```

---

## Task 2: `.github/workflows/publish-metadata.yml` — Job 2

**Files:**
- Create: `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/.github/workflows/publish-metadata.yml`

- [ ] **Step 1: Create the workflow**

Write `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/.github/workflows/publish-metadata.yml`:

```yaml
name: Publish metadata

# Triggered when Derek manually promotes the draft Release created by
# release.yml. Also supports manual re-run via workflow_dispatch for
# recovery when the tap PR needs a second attempt.
on:
  release:
    types: [published]
  workflow_dispatch:
    inputs:
      tag:
        description: "Release tag to reconcile (e.g. v0.1.0)"
        required: true

permissions:
  contents: write   # push to gh-pages
  issues: write     # open failure-tracking issue if tap PR fails

jobs:
  publish-metadata:
    name: Regenerate appcast + open Homebrew tap PR
    runs-on: ubuntu-latest
    timeout-minutes: 10

    steps:
      - name: Resolve tag
        id: tag
        run: |
          if [ -n "${{ github.event.inputs.tag }}" ]; then
            TAG="${{ github.event.inputs.tag }}"
          else
            TAG="${{ github.event.release.tag_name }}"
          fi
          VERSION="${TAG#v}"
          echo "tag=$TAG" >> "$GITHUB_OUTPUT"
          echo "version=$VERSION" >> "$GITHUB_OUTPUT"

      - name: Checkout main (for helper scripts)
        uses: actions/checkout@v4
        with:
          ref: main

      - name: Download Release artifacts
        env:
          GH_TOKEN: ${{ github.token }}
        run: |
          mkdir -p /tmp/release-assets
          cd /tmp/release-assets
          gh release download "${{ steps.tag.outputs.tag }}" \
            --repo "$GITHUB_REPOSITORY" \
            --pattern '*.sig' \
            --pattern '*.dmg' \
            --pattern '*.tar.gz' \
            --pattern 'checksums.txt'
          ls -la

      - name: Parse artifact metadata
        id: meta
        run: |
          VERSION="${{ steps.tag.outputs.version }}"
          TAG="${{ steps.tag.outputs.tag }}"
          ASSETS=/tmp/release-assets

          # Sparkle EdDSA signature + zip length
          SIG=$(cat "${ASSETS}/ContextFS-${VERSION}.zip.sig" | tr -d '[:space:]')
          ZIP_LEN=$(stat -c%s "${ASSETS}/ContextFS-${VERSION}.zip" 2>/dev/null || \
                    stat -f%z "${ASSETS}/ContextFS-${VERSION}.zip")

          # SHA-256 per artifact (read from checksums.txt)
          dmg_sha=$(grep "ContextFS-${VERSION}.dmg" "${ASSETS}/checksums.txt" | awk '{print $1}')
          arm_sha=$(grep "ctxfs-${VERSION}-darwin-arm64.tar.gz" "${ASSETS}/checksums.txt" | awk '{print $1}')
          x86_sha=$(grep "ctxfs-${VERSION}-darwin-x86_64.tar.gz" "${ASSETS}/checksums.txt" | awk '{print $1}')

          # Fetch release body for appcast description
          gh release view "$TAG" --repo "$GITHUB_REPOSITORY" --json body -q .body > /tmp/release-body.md

          echo "ed_sig=$SIG" >> "$GITHUB_OUTPUT"
          echo "zip_len=$ZIP_LEN" >> "$GITHUB_OUTPUT"
          echo "dmg_sha=$dmg_sha" >> "$GITHUB_OUTPUT"
          echo "arm_sha=$arm_sha" >> "$GITHUB_OUTPUT"
          echo "x86_sha=$x86_sha" >> "$GITHUB_OUTPUT"
        env:
          GH_TOKEN: ${{ github.token }}

      # -------- Appcast regeneration ---------------------------------

      - name: Checkout gh-pages
        uses: actions/checkout@v4
        with:
          ref: gh-pages
          path: gh-pages

      - name: Regenerate appcast.xml
        run: |
          VERSION="${{ steps.tag.outputs.version }}"
          TAG="${{ steps.tag.outputs.tag }}"
          ZIP_URL="${GITHUB_SERVER_URL}/${GITHUB_REPOSITORY}/releases/download/${TAG}/ContextFS-${VERSION}.zip"
          python3 scripts/append-appcast-item.py \
            --appcast gh-pages/appcast.xml \
            --version "$VERSION" \
            --short-version "$VERSION" \
            --enclosure-url "$ZIP_URL" \
            --ed-signature "${{ steps.meta.outputs.ed_sig }}" \
            --length "${{ steps.meta.outputs.zip_len }}" \
            --description-file /tmp/release-body.md
          xmllint --noout gh-pages/appcast.xml

      - name: Commit + push gh-pages
        run: |
          cd gh-pages
          git config user.name "github-actions[bot]"
          git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
          git add appcast.xml
          if git diff --staged --quiet; then
            echo "appcast.xml unchanged (tag may have been re-published)"
          else
            git commit -m "chore(appcast): add ${{ steps.tag.outputs.tag }}"
            git push origin gh-pages
          fi

      # -------- Homebrew tap bump ------------------------------------

      - name: Clone homebrew-ctxfs
        env:
          GH_TOKEN: ${{ secrets.HOMEBREW_TAP_PAT }}
        run: |
          gh repo clone Derek-X-Wang/homebrew-ctxfs /tmp/homebrew-ctxfs
          cd /tmp/homebrew-ctxfs
          git config user.name "github-actions[bot]"
          git config user.email "41898282+github-actions[bot]@users.noreply.github.com"

      - name: Render + stage cask + formula
        id: tap_bump
        continue-on-error: true
        env:
          GH_TOKEN: ${{ secrets.HOMEBREW_TAP_PAT }}
        run: |
          VERSION="${{ steps.tag.outputs.version }}"
          TAG="${{ steps.tag.outputs.tag }}"
          BRANCH="bump-${TAG}"

          cd /tmp/homebrew-ctxfs
          git checkout -B "$BRANCH"

          python3 "${GITHUB_WORKSPACE}/scripts/render-homebrew.py" \
            --version "$VERSION" \
            --tag "$TAG" \
            --repo-slug "$GITHUB_REPOSITORY" \
            --dmg-sha "${{ steps.meta.outputs.dmg_sha }}" \
            --arm-sha "${{ steps.meta.outputs.arm_sha }}" \
            --x86-sha "${{ steps.meta.outputs.x86_sha }}" \
            --cask-out Casks/contextfs.rb \
            --formula-out Formula/contextfs.rb

          git add Casks/contextfs.rb Formula/contextfs.rb
          if git diff --staged --quiet; then
            echo "cask + formula unchanged"
            exit 0
          fi

          git commit -m "Bump contextfs to ${TAG}"
          git push origin "$BRANCH" --force-with-lease
          gh pr create \
            --repo Derek-X-Wang/homebrew-ctxfs \
            --head "$BRANCH" \
            --base main \
            --title "Bump contextfs to ${TAG}" \
            --body "Auto-generated by Derek-X-Wang/ctxfs release.yml run ${GITHUB_RUN_ID}."

      - name: Open tracking issue if tap bump failed
        if: steps.tap_bump.outcome == 'failure'
        env:
          GH_TOKEN: ${{ github.token }}
        run: |
          RUN_URL="${GITHUB_SERVER_URL}/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}"
          gh issue create \
            --repo "$GITHUB_REPOSITORY" \
            --title "Tap bump failed for ${{ steps.tag.outputs.tag }}" \
            --body "Job 2 failed at the tap-bump step. Workflow run: ${RUN_URL}"

      - name: Re-raise tap-bump failure
        if: steps.tap_bump.outcome == 'failure'
        run: exit 1
```

- [ ] **Step 2: Validate with actionlint**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
actionlint .github/workflows/publish-metadata.yml
```

Expected: silent/clean.

- [ ] **Step 3: Commit**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
git add .github/workflows/publish-metadata.yml
git commit -m "feat(ci): publish-metadata.yml — Job 2 appcast + tap bump

Triggered on release: published (when Derek manually publishes the
draft created by release.yml) OR workflow_dispatch with a tag
input for recovery.

Steps: resolve tag, checkout main (for helper scripts), download
release assets via gh, parse EdDSA sig + SHA-256s + zip length,
checkout gh-pages, regenerate appcast.xml via helper script,
commit + push. Then clone homebrew-ctxfs via HOMEBREW_TAP_PAT,
render cask + formula via helper script, push branch, open PR.

Tap-bump step is wrapped in continue-on-error. On failure, opens
a tracking issue on the main repo, then re-raises so the workflow
is marked failed.

Permissions: contents:write (gh-pages push) + issues:write
(failure tracking). Cross-repo auth uses HOMEBREW_TAP_PAT secret."
```

---

## Task 3: `scripts/render-homebrew.py` + unit tests

**Files:**
- Create: `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/scripts/render-homebrew.py`
- Create: `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/tests/scripts/test_render_homebrew.py`

Pure stdlib Python 3. Keep it simple: template strings with f-string-style substitution. The Python script is the source of truth for cask + formula output shape so the YAML workflow doesn't need to carry Ruby heredocs.

- [ ] **Step 1: Create the renderer**

Write `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/scripts/render-homebrew.py`:

```python
#!/usr/bin/env python3
"""Render Casks/contextfs.rb and Formula/contextfs.rb from release metadata.

Called by the publish-metadata workflow (Phase 3d) with version + SHA-256s
and the source repo slug. Writes two Ruby files that Homebrew parses.

Stdlib only — no PyPI deps.
"""

import argparse
import os
import sys
import textwrap


CASK_TEMPLATE = textwrap.dedent("""\
    cask "contextfs" do
      version "{version}"
      sha256 "{dmg_sha}"

      url "https://github.com/{repo_slug}/releases/download/{tag}/ContextFS-#{{version}}.dmg"
      name "ContextFS"
      desc "AI-native mountable filesystem for Git repos and package registries"
      homepage "https://github.com/{repo_slug}"

      app "ContextFS.app"
      binary "#{{appdir}}/ContextFS.app/Contents/MacOS/ctxfs"

      conflicts_with formula: "contextfs"

      zap trash: [
        "~/.ctxfs",
        "~/Library/LaunchAgents/ai.ctxfs.daemon.plist",
        "~/Library/Preferences/ai.ctxfs.companion.plist",
      ]
    end
""")


FORMULA_TEMPLATE = textwrap.dedent("""\
    class Contextfs < Formula
      desc "AI-native mountable filesystem for Git repos and package registries"
      homepage "https://github.com/{repo_slug}"
      version "{version}"
      license "MIT OR Apache-2.0"

      on_macos do
        on_arm do
          url "https://github.com/{repo_slug}/releases/download/{tag}/ctxfs-#{{version}}-darwin-arm64.tar.gz"
          sha256 "{arm_sha}"
        end
        on_intel do
          url "https://github.com/{repo_slug}/releases/download/{tag}/ctxfs-#{{version}}-darwin-x86_64.tar.gz"
          sha256 "{x86_sha}"
        end
      end

      conflicts_with cask: "contextfs"

      def install
        bin.install "ctxfs"
      end

      test do
        system "#{{bin}}/ctxfs", "--help"
      end
    end
""")


def render_cask(*, version: str, tag: str, repo_slug: str, dmg_sha: str) -> str:
    return CASK_TEMPLATE.format(
        version=version,
        tag=tag,
        repo_slug=repo_slug,
        dmg_sha=dmg_sha,
    )


def render_formula(
    *, version: str, tag: str, repo_slug: str, arm_sha: str, x86_sha: str
) -> str:
    return FORMULA_TEMPLATE.format(
        version=version,
        tag=tag,
        repo_slug=repo_slug,
        arm_sha=arm_sha,
        x86_sha=x86_sha,
    )


def _validate_sha(name: str, value: str) -> None:
    if len(value) != 64 or not all(c in "0123456789abcdef" for c in value.lower()):
        raise ValueError(f"--{name} must be a 64-char hex SHA-256, got {value!r}")


def main() -> int:
    p = argparse.ArgumentParser(description="Render Homebrew cask + formula")
    p.add_argument("--version", required=True)
    p.add_argument("--tag", required=True)
    p.add_argument("--repo-slug", required=True, help="e.g. Derek-X-Wang/ctxfs")
    p.add_argument("--dmg-sha", required=True)
    p.add_argument("--arm-sha", required=True)
    p.add_argument("--x86-sha", required=True)
    p.add_argument("--cask-out", required=True, help="path to write Casks/contextfs.rb")
    p.add_argument("--formula-out", required=True, help="path to write Formula/contextfs.rb")
    args = p.parse_args()

    for field in ("dmg_sha", "arm_sha", "x86_sha"):
        _validate_sha(field.replace("_", "-"), getattr(args, field))

    if not args.tag.startswith("v"):
        print(f"error: --tag must start with 'v', got {args.tag!r}", file=sys.stderr)
        return 2

    if args.tag[1:] != args.version:
        print(
            f"error: --tag ({args.tag!r}) and --version ({args.version!r}) must agree",
            file=sys.stderr,
        )
        return 2

    cask = render_cask(
        version=args.version,
        tag=args.tag,
        repo_slug=args.repo_slug,
        dmg_sha=args.dmg_sha,
    )
    formula = render_formula(
        version=args.version,
        tag=args.tag,
        repo_slug=args.repo_slug,
        arm_sha=args.arm_sha,
        x86_sha=args.x86_sha,
    )

    os.makedirs(os.path.dirname(args.cask_out), exist_ok=True)
    os.makedirs(os.path.dirname(args.formula_out), exist_ok=True)
    with open(args.cask_out, "w") as f:
        f.write(cask)
    with open(args.formula_out, "w") as f:
        f.write(formula)

    print(f"wrote {args.cask_out}")
    print(f"wrote {args.formula_out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
```

- [ ] **Step 2: Make it executable**

```bash
chmod +x /Users/derekxwang/Development/incubator/ContextFS/ctxfs/scripts/render-homebrew.py
```

- [ ] **Step 3: Create unit tests**

Write `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/tests/scripts/test_render_homebrew.py`:

```python
"""Unit tests for scripts/render-homebrew.py."""

import importlib.util
import pathlib
import sys
import unittest


def _load():
    repo_root = pathlib.Path(__file__).resolve().parents[2]
    spec = importlib.util.spec_from_file_location(
        "render_homebrew", repo_root / "scripts" / "render-homebrew.py"
    )
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


render_homebrew = _load()

VALID_SHA = "0" * 64
OTHER_SHA = "a" * 64
THIRD_SHA = "f" * 64


class RenderCaskTests(unittest.TestCase):
    def test_emits_version_and_sha(self):
        out = render_homebrew.render_cask(
            version="0.1.0",
            tag="v0.1.0",
            repo_slug="Derek-X-Wang/ctxfs",
            dmg_sha=VALID_SHA,
        )
        self.assertIn('version "0.1.0"', out)
        self.assertIn(f'sha256 "{VALID_SHA}"', out)
        self.assertIn(
            "https://github.com/Derek-X-Wang/ctxfs/releases/download/v0.1.0/ContextFS-",
            out,
        )

    def test_emits_conflicts_and_zap(self):
        out = render_homebrew.render_cask(
            version="0.1.0", tag="v0.1.0",
            repo_slug="Derek-X-Wang/ctxfs", dmg_sha=VALID_SHA,
        )
        self.assertIn('conflicts_with formula: "contextfs"', out)
        self.assertIn("zap trash:", out)
        self.assertIn('"~/.ctxfs"', out)

    def test_binary_stanza_points_at_bundled_ctxfs(self):
        out = render_homebrew.render_cask(
            version="0.1.0", tag="v0.1.0",
            repo_slug="Derek-X-Wang/ctxfs", dmg_sha=VALID_SHA,
        )
        self.assertIn('binary "#{appdir}/ContextFS.app/Contents/MacOS/ctxfs"', out)


class RenderFormulaTests(unittest.TestCase):
    def test_emits_per_arch_urls(self):
        out = render_homebrew.render_formula(
            version="0.1.0", tag="v0.1.0",
            repo_slug="Derek-X-Wang/ctxfs",
            arm_sha=OTHER_SHA, x86_sha=THIRD_SHA,
        )
        self.assertIn("on_arm do", out)
        self.assertIn("on_intel do", out)
        self.assertIn("darwin-arm64", out)
        self.assertIn("darwin-x86_64", out)
        self.assertIn(f'sha256 "{OTHER_SHA}"', out)
        self.assertIn(f'sha256 "{THIRD_SHA}"', out)

    def test_emits_reciprocal_cask_conflict(self):
        out = render_homebrew.render_formula(
            version="0.1.0", tag="v0.1.0",
            repo_slug="Derek-X-Wang/ctxfs",
            arm_sha=OTHER_SHA, x86_sha=THIRD_SHA,
        )
        self.assertIn('conflicts_with cask: "contextfs"', out)


class ValidateShaTests(unittest.TestCase):
    def test_rejects_short(self):
        with self.assertRaises(ValueError):
            render_homebrew._validate_sha("x", "abc")

    def test_rejects_non_hex(self):
        with self.assertRaises(ValueError):
            render_homebrew._validate_sha("x", "z" * 64)

    def test_accepts_valid(self):
        render_homebrew._validate_sha("x", VALID_SHA)


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 4: Create a pytest-compatible conftest**

```bash
mkdir -p /Users/derekxwang/Development/incubator/ContextFS/ctxfs/tests/scripts
touch /Users/derekxwang/Development/incubator/ContextFS/ctxfs/tests/scripts/__init__.py
```

- [ ] **Step 5: Run the tests**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
python3 -m unittest tests.scripts.test_render_homebrew -v 2>&1 | tail -15
```

Expected: `OK` with 9 tests run.

- [ ] **Step 6: Commit**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
git add scripts/render-homebrew.py tests/scripts/
git commit -m "feat(ci): scripts/render-homebrew.py renders cask + formula

Python stdlib-only helper invoked by publish-metadata.yml. Takes
version + tag + SHA-256s + repo slug, writes Casks/contextfs.rb
and Formula/contextfs.rb with cross-channel conflict stanzas.

9 unit tests cover cask output (URL, SHA, conflicts, zap stanza,
binary stanza), formula output (per-arch URLs, SHAs, reciprocal
conflict), and SHA-256 format validation.

Cask binary stanza symlinks the bundled ctxfs into HOMEBREW_PREFIX/bin
so 'brew install --cask contextfs' + PATH just works.

zap stanza removes ~/.ctxfs, launchd plist, and preferences on
uninstall."
```

---

## Task 4: `scripts/append-appcast-item.py` + unit tests

**Files:**
- Create: `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/scripts/append-appcast-item.py`
- Create: `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/tests/scripts/test_append_appcast_item.py`

Prepends a new `<item>` to an existing `appcast.xml`'s first `<channel>`. XML escaping via `xml.sax.saxutils.escape` (stdlib). Does not use `lxml` — stdlib `xml.etree.ElementTree` is enough and doesn't need a PyPI dep.

- [ ] **Step 1: Create the script**

Write `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/scripts/append-appcast-item.py`:

```python
#!/usr/bin/env python3
"""Prepend a new <item> to an existing appcast.xml's first <channel>.

Called by publish-metadata.yml (Phase 3d) when a GitHub Release is published.

Stdlib only — no PyPI deps.
"""

import argparse
import email.utils
import sys
import xml.etree.ElementTree as ET
from xml.sax.saxutils import escape


SPARKLE_NS = "http://www.andymatuschak.org/xml-namespaces/sparkle"

# Register the sparkle namespace so ElementTree doesn't rewrite sparkle:foo
# into ns0:foo on serialize.
ET.register_namespace("sparkle", SPARKLE_NS)


def build_item_xml(
    *,
    version: str,
    short_version: str,
    enclosure_url: str,
    ed_signature: str,
    length: int,
    description_html: str,
    pub_date: str,
) -> ET.Element:
    """Return a <item> Element with the Sparkle-convention children."""
    item = ET.Element("item")

    title = ET.SubElement(item, "title")
    title.text = f"Version {short_version}"

    sparkle_version = ET.SubElement(item, f"{{{SPARKLE_NS}}}version")
    sparkle_version.text = str(version)

    sparkle_short = ET.SubElement(item, f"{{{SPARKLE_NS}}}shortVersionString")
    sparkle_short.text = short_version

    desc = ET.SubElement(item, "description")
    # Wrap in CDATA by hex-escaping — but ElementTree serializes text as
    # escaped characters, which browsers and Sparkle both accept. Don't
    # try to emit raw CDATA sections (stdlib doesn't support it cleanly).
    desc.text = description_html

    pub = ET.SubElement(item, "pubDate")
    pub.text = pub_date

    enc = ET.SubElement(item, "enclosure")
    enc.set("url", enclosure_url)
    enc.set("length", str(length))
    enc.set("type", "application/octet-stream")
    enc.set(f"{{{SPARKLE_NS}}}edSignature", ed_signature)

    return item


def append_item_to_appcast(
    appcast_path: str,
    item: ET.Element,
) -> None:
    """Parse the existing appcast.xml, prepend `item` to its first <channel>,
    and write the result back. Raises if the file doesn't match the
    expected RSS+Sparkle shape."""
    tree = ET.parse(appcast_path)
    root = tree.getroot()
    if root.tag != "rss":
        raise ValueError(f"expected <rss> root, got <{root.tag}>")

    channel = root.find("channel")
    if channel is None:
        raise ValueError("<channel> not found in appcast.xml")

    # Find the first existing <item> (if any) — we want to insert before it,
    # so newest-first ordering is preserved. If none exist, append to channel.
    existing_item = channel.find("item")
    if existing_item is not None:
        idx = list(channel).index(existing_item)
        channel.insert(idx, item)
    else:
        channel.append(item)

    tree.write(appcast_path, xml_declaration=True, encoding="utf-8")


def main() -> int:
    p = argparse.ArgumentParser(description="Prepend an item to appcast.xml")
    p.add_argument("--appcast", required=True, help="path to appcast.xml to modify in place")
    p.add_argument("--version", required=True, help="Sparkle numeric version (often monotonic int or semver)")
    p.add_argument("--short-version", required=True, help="User-facing version string")
    p.add_argument("--enclosure-url", required=True)
    p.add_argument("--ed-signature", required=True)
    p.add_argument("--length", required=True, type=int)
    p.add_argument("--description-file", required=True, help="markdown/html file with release notes")
    p.add_argument("--pub-date", default=None, help="RFC 2822 date (default: now)")
    args = p.parse_args()

    with open(args.description_file) as f:
        description = f.read().strip()

    # Description field: pre-escape any XML-unsafe characters so that
    # browsers rendering the feed (and Sparkle itself) receive safe HTML.
    description_safe = escape(description)

    pub_date = args.pub_date or email.utils.formatdate(usegmt=True)

    item = build_item_xml(
        version=args.version,
        short_version=args.short_version,
        enclosure_url=args.enclosure_url,
        ed_signature=args.ed_signature,
        length=args.length,
        description_html=description_safe,
        pub_date=pub_date,
    )

    append_item_to_appcast(args.appcast, item)
    print(f"appended {args.short_version} to {args.appcast}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
```

- [ ] **Step 2: Make it executable**

```bash
chmod +x /Users/derekxwang/Development/incubator/ContextFS/ctxfs/scripts/append-appcast-item.py
```

- [ ] **Step 3: Create unit tests**

Write `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/tests/scripts/test_append_appcast_item.py`:

```python
"""Unit tests for scripts/append-appcast-item.py."""

import importlib.util
import pathlib
import tempfile
import unittest
import xml.etree.ElementTree as ET


def _load():
    repo_root = pathlib.Path(__file__).resolve().parents[2]
    spec = importlib.util.spec_from_file_location(
        "append_appcast", repo_root / "scripts" / "append-appcast-item.py"
    )
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


mod = _load()

SEED_APPCAST = """<?xml version='1.0' standalone='yes'?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>ContextFS Updates</title>
    <link>https://example.invalid/appcast.xml</link>
    <description>Updates</description>
    <language>en</language>
  </channel>
</rss>
"""


class BuildItemTests(unittest.TestCase):
    def test_item_has_sparkle_version_and_short_version(self):
        item = mod.build_item_xml(
            version="1",
            short_version="0.1.0",
            enclosure_url="https://example.invalid/app.zip",
            ed_signature="AA==",
            length=1,
            description_html="<p>Notes</p>",
            pub_date="Mon, 20 Apr 2026 00:00:00 GMT",
        )
        sv = item.find(f"{{{mod.SPARKLE_NS}}}version")
        ssv = item.find(f"{{{mod.SPARKLE_NS}}}shortVersionString")
        self.assertEqual(sv.text, "1")
        self.assertEqual(ssv.text, "0.1.0")

    def test_enclosure_attrs(self):
        item = mod.build_item_xml(
            version="1",
            short_version="0.1.0",
            enclosure_url="https://example.invalid/app.zip",
            ed_signature="SIG==",
            length=12345,
            description_html="notes",
            pub_date="Mon, 20 Apr 2026 00:00:00 GMT",
        )
        enc = item.find("enclosure")
        self.assertEqual(enc.get("url"), "https://example.invalid/app.zip")
        self.assertEqual(enc.get("length"), "12345")
        self.assertEqual(enc.get("type"), "application/octet-stream")
        self.assertEqual(
            enc.get(f"{{{mod.SPARKLE_NS}}}edSignature"), "SIG=="
        )


class AppendTests(unittest.TestCase):
    def _seed(self) -> str:
        tmp = tempfile.NamedTemporaryFile(
            mode="w", suffix=".xml", delete=False, encoding="utf-8"
        )
        tmp.write(SEED_APPCAST)
        tmp.close()
        return tmp.name

    def _item(self) -> ET.Element:
        return mod.build_item_xml(
            version="1",
            short_version="0.1.0",
            enclosure_url="https://example.invalid/app.zip",
            ed_signature="AA==",
            length=1,
            description_html="notes",
            pub_date="Mon, 20 Apr 2026 00:00:00 GMT",
        )

    def test_appends_to_empty_channel(self):
        path = self._seed()
        mod.append_item_to_appcast(path, self._item())
        tree = ET.parse(path)
        items = tree.getroot().findall("./channel/item")
        self.assertEqual(len(items), 1)

    def test_prepends_when_item_exists(self):
        path = self._seed()
        mod.append_item_to_appcast(path, self._item())  # first (for v0.1.0)
        # Second call with a different short-version goes at the top (newest first)
        second = mod.build_item_xml(
            version="2",
            short_version="0.2.0",
            enclosure_url="https://example.invalid/v2.zip",
            ed_signature="BB==",
            length=2,
            description_html="v2 notes",
            pub_date="Tue, 21 Apr 2026 00:00:00 GMT",
        )
        mod.append_item_to_appcast(path, second)

        tree = ET.parse(path)
        items = tree.getroot().findall("./channel/item")
        self.assertEqual(len(items), 2)
        first_title = items[0].find("title").text
        self.assertEqual(first_title, "Version 0.2.0")
        second_title = items[1].find("title").text
        self.assertEqual(second_title, "Version 0.1.0")

    def test_rejects_non_rss_root(self):
        tmp = tempfile.NamedTemporaryFile(
            mode="w", suffix=".xml", delete=False, encoding="utf-8"
        )
        tmp.write("<?xml version='1.0'?><nothing/>\n")
        tmp.close()
        with self.assertRaises(ValueError):
            mod.append_item_to_appcast(tmp.name, self._item())


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 4: Run the tests**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
python3 -m unittest tests.scripts.test_append_appcast_item -v 2>&1 | tail -15
```

Expected: `OK` with 5 tests run.

- [ ] **Step 5: Commit**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
git add scripts/append-appcast-item.py tests/scripts/test_append_appcast_item.py
git commit -m "feat(ci): scripts/append-appcast-item.py prepends to appcast.xml

Python stdlib-only helper invoked by publish-metadata.yml. Takes
version metadata + EdDSA sig + description-file path, builds a
Sparkle <item> via xml.etree, and inserts it at the top of the
existing <channel> so newest-first ordering is preserved.

XML escape safety via xml.sax.saxutils.escape on the description
text. Register sparkle namespace so ET doesn't rewrite prefixes
on serialize.

5 unit tests cover item construction (sparkle:version, sparkle:
shortVersionString, enclosure attrs including edSignature), append
behavior (empty channel, non-empty channel ordering), and rejection
of non-RSS input files."
```

---

## Task 5: Dress-rehearsal release notes

**Files:**
- Create: `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/.github/release-notes/v0.0.1-rc1.md`

Phase 3e's first run will push `v0.0.1-rc1` before `v0.1.0` to shake out the pipeline. Needs a notes file or Job 1 aborts. **But** the release.sh script and CI both reject any tag that doesn't match plain `vX.Y.Z` semver (no suffixes). So a "rc1" tag can't actually be pushed via the current tooling.

Decision: change the dress-rehearsal approach to use `v0.0.1` (plain semver, treated as a throwaway patch release) instead of `v0.0.1-rc1`. Same safety net — Derek can delete the draft if anything looks wrong and push `v0.1.0` for the real first release.

- [ ] **Step 1: Create the dress-rehearsal notes**

Write `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/.github/release-notes/v0.0.1.md`:

```markdown
# ContextFS 0.0.1 (dress rehearsal)

**This is a Phase 3e pipeline dress rehearsal, not a real release.**
Do not install. The draft Release created by this tag will be discarded
after Derek confirms the pipeline produced correctly signed, notarized
artifacts.

If you somehow found this through GitHub Releases, skip to `v0.1.0` for
the actual first release.
```

- [ ] **Step 2: Commit**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
git add .github/release-notes/v0.0.1.md
git commit -m "docs(release): v0.0.1 dress-rehearsal notes

Phase 3e runs this tag first to shake out the CI pipeline.
Notes file is required by release.yml's 'Assert release notes
exist' step. Throwaway — draft release gets discarded before
the real v0.1.0 cut."
```

---

## Task 6: Pipeline validation

- [ ] **Step 1: actionlint both workflows**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
actionlint .github/workflows/release.yml .github/workflows/publish-metadata.yml
```

Expected: no output (actionlint is silent on clean files). Address any shellcheck or YAML syntax complaints it raises.

- [ ] **Step 2: Full Python test suite**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
python3 -m unittest discover tests/scripts -v 2>&1 | tail -20
```

Expected: `OK` with 14 tests run (9 render_homebrew + 5 append_appcast).

- [ ] **Step 3: Exercise each helper script end-to-end against stub data**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
STUB_SHA="0000000000000000000000000000000000000000000000000000000000000000"
mkdir -p /tmp/homebrew-dry-run/Casks /tmp/homebrew-dry-run/Formula
python3 scripts/render-homebrew.py \
  --version 0.1.0 --tag v0.1.0 \
  --repo-slug Derek-X-Wang/ctxfs \
  --dmg-sha "$STUB_SHA" --arm-sha "$STUB_SHA" --x86-sha "$STUB_SHA" \
  --cask-out /tmp/homebrew-dry-run/Casks/contextfs.rb \
  --formula-out /tmp/homebrew-dry-run/Formula/contextfs.rb
echo "--- cask ---"
cat /tmp/homebrew-dry-run/Casks/contextfs.rb
echo "--- formula ---"
cat /tmp/homebrew-dry-run/Formula/contextfs.rb
```

Expected: both files written, cask has `app "ContextFS.app"` + binary stanza, formula has per-arch URLs.

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
cat > /tmp/appcast-seed.xml <<'EOF'
<?xml version="1.0" standalone="yes"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>ContextFS</title>
  </channel>
</rss>
EOF
cat > /tmp/release-body.md <<'EOF'
## Test notes
Something happened.
EOF
python3 scripts/append-appcast-item.py \
  --appcast /tmp/appcast-seed.xml \
  --version 1 --short-version 0.1.0 \
  --enclosure-url https://example.invalid/zip \
  --ed-signature TESTSIG \
  --length 42 \
  --description-file /tmp/release-body.md
xmllint --noout /tmp/appcast-seed.xml && echo "XML is well-formed"
cat /tmp/appcast-seed.xml
```

Expected: `XML is well-formed`; the printed XML has an `<item>` with `<sparkle:version>1</sparkle:version>` and `<sparkle:shortVersionString>0.1.0</sparkle:shortVersionString>`.

- [ ] **Step 4: Commit if actionlint required any workflow tweaks**

If Steps 1–3 were clean, there's nothing to commit. If actionlint flagged issues and you fixed them, commit the fixes referencing this task.

---

## Self-review checklist

**Spec coverage:** Plan 3d covers spec Section 3 (GitHub Actions pipeline) end-to-end. Both Job 1 and Job 2 map to spec sub-sections. Pre-release validation (spctl, codesign --verify, clippy, tests, --version match) is in place. `workflow_dispatch` recovery path is in Job 2. Secrets required are explicitly named. Helper scripts are unit-tested.

**Placeholder scan:** No "TBD"/"TODO"/"fill in later" in the workflow YAMLs or helper scripts. The one `SPARKLE_TARBALL_SHA256` env var is intentionally empty until Phase 3e bootstrap pins it — the workflow warns but doesn't fail if unset, so 3d still produces a working pipeline that 3e just tightens.

**Type consistency:** Argument names match across the Python scripts, their unit tests, and the workflow invocations that feed them (e.g., `--version`, `--tag`, `--dmg-sha`, `--arm-sha`, `--x86-sha`, `--cask-out`, `--formula-out`, `--appcast`, `--enclosure-url`, `--ed-signature`, `--length`, `--description-file`). Tag format (`v` prefix) vs version format (no prefix) is consistent: scripts assert `tag[1:] == version` and CI derives both from `GITHUB_REF_NAME`.

**Known edges the plan does NOT solve** (all deferred with explicit citations above):
- Secrets are *read*; Phase 3e *creates* them (Apple Developer portal, Sparkle keys, tap repo, gh-pages seed, HOMEBREW_TAP_PAT).
- Actually running the workflow requires Phase 3e's bootstrap complete. 3d produces the pipeline code; 3e tests it end-to-end via the `v0.0.1` dress rehearsal.
- `SPARKLE_TARBALL_SHA256` pin is wired but empty; 3e fills it on first successful run.
- `minisign` verification of CLI tarballs is not implemented — Phase 3 spec explicitly drops it.
