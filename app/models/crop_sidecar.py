"""
Sidecar helpers for persistent crop rectangles.

`.photo-pipeline-crops.json` lives in the scanned folder root next to the
prune / pair / export sidecars and stores one normalised rect per file:

    {
      "crops": {
        "RAW/IMG_0042.cr3": {"left": 0.08, "top": 0.0, "right": 0.92,
                             "bottom": 1.0, "aspect": "1:1"},
        "JPG/IMG_0042.jpg": {...}
      }
    }

Paths are relative to the folder root so the sidecar survives the folder being
moved, and both halves of a RAW+JPG pair get their own entry — the viewer keeps
them in step, but the file stays readable and repairable by hand if one half is
later removed.
"""
from __future__ import annotations

import json
from pathlib import Path
from typing import Dict

from app.models.crop import Crop
from app.models.sidecar import atomic_write_text

_CROPS_FILE = ".photo-pipeline-crops.json"


def read_crops(folder: Path) -> Dict[Path, Crop]:
    """
    Read persisted crops, returning {absolute_path: Crop}.

    A missing or malformed sidecar yields an empty dict — never raises.
    Individual unparseable entries are skipped rather than failing the load.
    """
    sidecar = folder / _CROPS_FILE
    if not sidecar.exists():
        return {}
    try:
        data = json.loads(sidecar.read_text())
    except Exception:
        return {}

    result: Dict[Path, Crop] = {}
    for rel, entry in (data.get("crops") or {}).items():
        if not isinstance(entry, dict):
            continue
        crop = Crop.from_dict(entry)
        if crop is None:
            continue
        p = Path(rel)
        result[p if p.is_absolute() else folder / p] = crop
    return result


def write_crops(folder: Path, crops: Dict[Path, Crop]) -> None:
    """
    Persist {absolute_path: Crop} to the sidecar.

    Full-frame crops are dropped — an uncropped file needs no entry, and this
    keeps the sidecar from growing an entry per photo just from opening the
    crop tool and cancelling.
    """
    entries = {}
    for path, crop in crops.items():
        if crop is None or crop.is_full:
            continue
        try:
            rel = str(path.relative_to(folder))
        except ValueError:
            rel = str(path)       # outside the root — store absolute
        entries[rel] = crop.to_dict()

    target = folder / _CROPS_FILE
    if not entries:
        # Nothing left to remember; remove a stale sidecar rather than
        # leaving an empty one behind.
        target.unlink(missing_ok=True)
        return

    try:
        atomic_write_text(
            target,
            json.dumps({"crops": dict(sorted(entries.items()))}, indent=2),
        )
    except Exception as exc:
        raise OSError(f"Could not save crops: {exc}") from exc
