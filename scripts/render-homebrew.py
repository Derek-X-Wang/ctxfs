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

      # No conflicts_with stanza on the cask: Homebrew Cask only accepts
      # `conflicts_with cask:`, not `formula:`. The reciprocal declaration
      # on Formula/contextfs.rb (`conflicts_with cask: "contextfs"`) is
      # sufficient — brew refuses to install the formula when this cask
      # is present.

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
