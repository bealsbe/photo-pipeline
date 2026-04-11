"""
Icon loader for the pixel-icon-library.

Uses the pre-rendered PNG sets (dark-mode = light icons, for our dark theme).
Returns an empty QIcon silently if the file is missing so the rest of the
UI degrades gracefully to text-only.

Usage
-----
    from app.ui.icons import icon
    action.setIcon(icon("file-import"))
    button.setIcon(icon("trash-alt", size=16))
"""
from __future__ import annotations

from pathlib import Path

from PySide6.QtGui import QIcon, QPixmap

_ICON_ROOT = Path("/home/beals/Desktop/src/pixel-icon-library/icons/PNG")

# Our app has a dark background → use the "for-dark-mode" set
# (icons are rendered in light/white tones so they're visible on dark surfaces)
_DARK = _ICON_ROOT / "for-dark-mode"


def icon(name: str, size: int = 24, style: str = "regular") -> QIcon:
    """
    Load a named icon as a QIcon.

    Parameters
    ----------
    name  : icon filename stem, e.g. "file-import", "trash", "grid"
    size  : 12, 16, 24, or 48  (pixels)
    style : "regular" or "solid"
    """
    path = _DARK / f"{size}px" / style / f"{name}.png"
    if not path.exists():
        return QIcon()
    return QIcon(str(path))


def pixmap(name: str, size: int = 24, style: str = "regular") -> QPixmap:
    """Same as icon() but returns a QPixmap directly."""
    path = _DARK / f"{size}px" / style / f"{name}.png"
    if not path.exists():
        return QPixmap()
    return QPixmap(str(path))
