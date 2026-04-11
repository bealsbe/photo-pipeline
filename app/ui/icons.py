"""
Icon loader — reads PNGs bundled under app/ui/icons/.

Layout on disk:
    app/ui/icons/
        12px/  16px/  24px/  48px/
            <name>.png   (one file per icon name, all light/white for dark UI)

Returns an empty QIcon silently if a file is missing so the rest of the
UI degrades gracefully to text-only.

Usage
-----
    from app.ui.icons import icon
    action.setIcon(icon("file-import"))
    button.setIcon(icon("trash-alt", size=16))
"""
from __future__ import annotations

from pathlib import Path

from PySide6.QtCore import Qt
from PySide6.QtGui import QColor, QIcon, QPainter, QPixmap

# Icons are stored alongside this module: app/ui/icons/<size>px/<name>.png
_ICON_DIR = Path(__file__).parent / "icons"


def _closest_size(size: int) -> int:
    """Return the nearest available bucket (12, 16, 24, 48)."""
    for bucket in (12, 16, 24, 48):
        if size <= bucket:
            return bucket
    return 48


def icon(name: str, size: int = 24, style: str = "regular") -> QIcon:
    """
    Load a named icon as a QIcon.

    Parameters
    ----------
    name  : icon filename stem, e.g. "file-import", "trash", "grid"
    size  : requested pixel size — snapped to the nearest available bucket
    style : ignored (kept for API compatibility)
    """
    path = _ICON_DIR / f"{_closest_size(size)}px" / f"{name}.png"
    if not path.exists():
        return QIcon()
    return QIcon(str(path))


def pixmap(name: str, size: int = 24, style: str = "regular") -> QPixmap:
    """Same as icon() but returns a QPixmap directly."""
    path = _ICON_DIR / f"{_closest_size(size)}px" / f"{name}.png"
    if not path.exists():
        return QPixmap()
    return QPixmap(str(path))


def tinted_icon(
    name: str,
    tint: str = "#ff6d00",
    size: int = 48,
    style: str = "regular",
) -> QIcon:
    """
    Load an icon and overlay a solid colour tint using SourceIn composition.
    The result retains the original alpha mask but all opaque pixels become
    the requested colour — good for colouring monochrome PNG icons.
    """
    src = pixmap(name, size, style)
    if src.isNull():
        return QIcon()
    tinted = QPixmap(src.size())
    tinted.fill(Qt.transparent)
    p = QPainter(tinted)
    p.drawPixmap(0, 0, src)
    p.setCompositionMode(QPainter.CompositionMode_SourceIn)
    p.fillRect(tinted.rect(), QColor(tint))
    p.end()
    return QIcon(tinted)


