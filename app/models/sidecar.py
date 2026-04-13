"""
Sidecar helpers for persistent RAW+JPG pair marks.

The sidecar file `.photo-pipeline-pairs.json` is stored in the folder root
alongside `.photo_pipeline.json` (prune marks).  It stores the set of
(canonical_parent_relative, lowercase_stem) tuples that have been explicitly
confirmed as pairs by the user.

Key design decisions:
- Canonical parent strips one level when the immediate parent is "RAW" or "JPG",
  so the keys are stable across Sort operations (which move files into those
  subfolders).
- All paths stored as strings relative to the folder root so the sidecar is
  portable if the folder is moved.
"""
from __future__ import annotations

import json
from pathlib import Path
from typing import Set, Tuple

_PAIRS_FILE = ".photo-pipeline-pairs.json"

PairKey = Tuple[str, str]   # (canonical_parent_abs_str, lowercase_stem)


def _canonical_parent(path: Path) -> Path:
    """Strip one RAW/JPG level — mirrors PhotoCollection._canonical_parent."""
    if path.parent.name.upper() in ("RAW", "JPG"):
        return path.parent.parent
    return path.parent


def read_paired_keys(folder: Path) -> Set[PairKey]:
    """
    Read persisted pair keys from sidecar.

    Returns a set of (canonical_parent_abs_str, stem) tuples, or an empty set
    if the sidecar doesn't exist or is malformed.
    """
    sidecar = folder / _PAIRS_FILE
    if not sidecar.exists():
        return set()
    try:
        data = json.loads(sidecar.read_text())
        result: Set[PairKey] = set()
        for entry in data.get("paired_stems", []):
            parent_rel = entry.get("parent", "")
            stem       = entry.get("stem", "")
            if stem:
                abs_parent = str(folder / parent_rel) if parent_rel else str(folder)
                result.add((abs_parent, stem.lower()))
        return result
    except Exception:
        return set()


def write_paired_keys(folder: Path, keys: Set[PairKey]) -> None:
    """
    Persist a set of (canonical_parent_abs_str, stem) pair keys to sidecar.

    Paths are stored relative to *folder* for portability.
    """
    try:
        entries = []
        for abs_parent_str, stem in sorted(keys):
            abs_parent = Path(abs_parent_str)
            try:
                rel = str(abs_parent.relative_to(folder))
            except ValueError:
                rel = abs_parent_str    # absolute fallback
            entries.append({"parent": rel, "stem": stem})
        data = {"paired_stems": entries}
        (folder / _PAIRS_FILE).write_text(json.dumps(data, indent=2))
    except Exception:
        pass
