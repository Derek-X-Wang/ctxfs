"""Shared test helpers for scripts/ tests.

`load_script(stem)` imports a script from the repo's `scripts/` directory
as a module. We load via `importlib.util` because the scripts have
dash-separated filenames (`render-homebrew.py`) which aren't valid Python
module identifiers, so the normal import machinery can't find them.
"""

import importlib.util
import pathlib
import types


_REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]


def load_script(stem: str) -> types.ModuleType:
    """Import `scripts/{stem}.py` as a module and return it.

    Example:
        render_homebrew = load_script("render-homebrew")
    """
    path = _REPO_ROOT / "scripts" / f"{stem}.py"
    module_name = stem.replace("-", "_")
    spec = importlib.util.spec_from_file_location(module_name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod
