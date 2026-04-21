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
