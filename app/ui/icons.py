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

from PySide6.QtCore import QPointF, QRectF, Qt
from PySide6.QtGui import QColor, QIcon, QPainter, QPen, QPixmap

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


def check_icon(color: str = "#3ac96a", size: int = 24) -> QIcon:
    """Procedurally draw a check-mark glyph as a QIcon."""
    pm = QPixmap(size, size)
    pm.fill(Qt.transparent)
    p = QPainter(pm)
    p.setRenderHint(QPainter.Antialiasing)
    pen = QPen(QColor(color))
    pen.setWidthF(max(1.6, size / 11))
    pen.setCapStyle(Qt.RoundCap)
    pen.setJoinStyle(Qt.RoundJoin)
    p.setPen(pen)
    # Check mark: down-stroke from upper-left, up-stroke to upper-right
    u = size / 24.0
    p.drawLine(int(5 * u), int(13 * u), int(10 * u), int(18 * u))
    p.drawLine(int(10 * u), int(18 * u), int(19 * u), int(7 * u))
    p.end()
    return QIcon(pm)


def calendar_icon(color: str = "#c8c8d8", size: int = 24) -> QIcon:
    """Procedurally draw a calendar glyph (page, binder rings, day dots)."""
    pm = QPixmap(size, size)
    pm.fill(Qt.transparent)
    p = QPainter(pm)
    p.setRenderHint(QPainter.Antialiasing)
    u = size / 24.0
    pen = QPen(QColor(color))
    pen.setWidthF(max(1.4, size / 14))
    pen.setCapStyle(Qt.RoundCap)
    p.setPen(pen)
    p.setBrush(Qt.NoBrush)
    p.drawRoundedRect(QRectF(3.5 * u, 5 * u, 17 * u, 15.5 * u), 2.5 * u, 2.5 * u)
    p.drawLine(QPointF(3.5 * u, 9.5 * u), QPointF(20.5 * u, 9.5 * u))
    p.drawLine(QPointF(8 * u, 3 * u), QPointF(8 * u, 6.5 * u))
    p.drawLine(QPointF(16 * u, 3 * u), QPointF(16 * u, 6.5 * u))
    p.setPen(Qt.NoPen)
    p.setBrush(QColor(color))
    r = 1.2 * u
    for row, y in enumerate((13 * u, 17 * u)):
        for col, x in enumerate((8 * u, 12 * u, 16 * u)):
            if row == 1 and col == 2:
                p.setBrush(QColor("#ff6d00"))   # one highlighted day
            p.drawEllipse(QPointF(x, y), r, r)
    p.end()
    return QIcon(pm)


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


