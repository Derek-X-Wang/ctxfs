"""Unit tests for scripts/render-homebrew.py."""

import unittest

from .conftest import load_script


render_homebrew = load_script("render-homebrew")

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

    def test_omits_cask_conflicts_with_and_emits_zap(self):
        # Homebrew Cask only accepts `conflicts_with cask:`, not `:formula`.
        # The reciprocal declaration lives on the formula side — neither form
        # of `conflicts_with` should appear as an actual stanza in the cask.
        # Match the keyword followed by space + `cask:` or `formula:` so a
        # comment that mentions the keyword doesn't trigger a false positive.
        out = render_homebrew.render_cask(
            version="0.1.0", tag="v0.1.0",
            repo_slug="Derek-X-Wang/ctxfs", dmg_sha=VALID_SHA,
        )
        self.assertNotRegex(out, r"^\s*conflicts_with\s+(cask|formula):")
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
