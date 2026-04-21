# scripts/

Repository release tooling.

## `release.sh`

Stamp a new version everywhere + create a commit and tag. Does **not** push.

```bash
scripts/release.sh 0.1.0
```

### What it does

1. Validates the argument is plain semver `X.Y.Z` (no `-rc`, no `+build` — Phase 3 doesn't use suffixes).
2. Asserts `git status` is clean (no stray dirty changes get included in the release commit).
3. Asserts `.github/release-notes/vX.Y.Z.md` exists and is non-empty (no un-annotated releases).
4. Asserts the tag doesn't already exist locally.
5. Writes `X.Y.Z` to `VERSION`.
6. Stamps `version = "X.Y.Z"` inside `[workspace.package]` of the root `Cargo.toml`.
7. Stamps `MARKETING_VERSION = X.Y.Z;` across `project.pbxproj` (every build config / target).
8. Stamps `CURRENT_PROJECT_VERSION` across `project.pbxproj` with `$(git rev-list --count HEAD)` — monotonic build number.
9. Runs `cargo generate-lockfile --offline` (falls back to online if needed) to refresh `Cargo.lock`.
10. `git add`s the explicit list, creates a `chore: release vX.Y.Z` commit, creates the `vX.Y.Z` tag.

### What it doesn't do

- **Doesn't push.** That's on you — last safety net before CI runs. Review the commit and tag first:
  ```bash
  git show HEAD
  git log -1 --stat
  # then, if happy:
  git push && git push --tags
  ```
- **Doesn't create `.github/release-notes/vX.Y.Z.md`.** Write it by hand and commit it *before* running this script.
- **Doesn't bump the `fskit-rs` crate version.** That's a vendored fork on its own version track.
- **Doesn't run tests or clippy.** CI does. Run `cargo test` + `cargo clippy` locally before bumping if you want extra confidence.

### Failure modes

| Exit code | Reason |
|---|---|
| 64 | Bad argument (missing or non-semver) |
| 65 | Precondition failed (dirty tree, missing notes, tag already exists) |
| Other | Cargo / sed / git failure — read stderr |

### Undoing a release cut

If you ran the script and something's wrong:

```bash
git tag -d vX.Y.Z
git reset --hard HEAD~1
```

Safe to do as long as you haven't pushed.
