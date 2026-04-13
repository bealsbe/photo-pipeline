"""
FolderBar — persistent two-path strip showing working folder and library folder.

Sits below the toolbar.  Clicking either path opens a folder picker.
A chevron on the right edge collapses the bar to a slim strip and back.
Visibility / collapsed state persists in QSettings.
"""
from __future__ import annotations

from pathlib import Path
from typing import Optional

from PySide6.QtCore import QDir, Qt, Signal
from PySide6.QtWidgets import (
    QFileDialog,
    QFrame,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QSizePolicy,
    QWidget,
)

_HEIGHT_EXPANDED  = 40
_HEIGHT_COLLAPSED = 24


def _elide(path: Path, max_len: int = 52) -> str:
    s = str(path)
    if len(s) <= max_len:
        return s
    return "…" + s[-(max_len - 1):]


class _FolderSlot(QWidget):
    """One clickable folder slot (icon + label + path)."""

    clicked = Signal()

    _QSS = (
        "QWidget#slot {"
        "  background: #0d0d1e;"
        "  border: 1px solid rgba(255,109,0,0.10);"
        "  border-radius: 5px;"
        "}"
        "QWidget#slot:hover {"
        "  background: rgba(255,109,0,0.06);"
        "  border-color: rgba(255,109,0,0.28);"
        "}"
    )

    def __init__(self, icon_text: str, label: str, parent=None) -> None:
        super().__init__(parent)
        self.setObjectName("slot")
        self.setStyleSheet(self._QSS)
        self.setCursor(Qt.PointingHandCursor)
        self.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Fixed)
        self.setFixedHeight(28)

        lay = QHBoxLayout(self)
        lay.setContentsMargins(8, 0, 8, 0)
        lay.setSpacing(5)

        icon_lbl = QLabel(icon_text)
        icon_lbl.setStyleSheet(
            "font-size: 13px; background: transparent; color: rgba(255,109,0,0.70);"
        )
        icon_lbl.setFixedWidth(16)

        type_lbl = QLabel(label)
        type_lbl.setStyleSheet(
            "font-size: 10px; font-weight: 700; color: #505070;"
            " background: transparent; letter-spacing: 0.5px;"
        )

        self._path_lbl = QLabel()
        self._path_lbl.setStyleSheet(
            "font-size: 11px; color: #9898b8; background: transparent;"
        )
        self._path_lbl.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Fixed)

        self._unset_lbl = QLabel("click to set…")
        self._unset_lbl.setStyleSheet(
            "font-size: 11px; color: #404058; font-style: italic; background: transparent;"
        )

        lay.addWidget(icon_lbl)
        lay.addWidget(type_lbl)
        lay.addWidget(self._path_lbl)
        lay.addWidget(self._unset_lbl)

        self._path: Optional[Path] = None
        self._refresh()

    def set_path(self, path: Optional[Path]) -> None:
        self._path = path
        self._refresh()

    def path(self) -> Optional[Path]:
        return self._path

    def _refresh(self) -> None:
        if self._path:
            self._path_lbl.setText(_elide(self._path))
            self._path_lbl.setToolTip(str(self._path))
            self._path_lbl.show()
            self._unset_lbl.hide()
        else:
            self._path_lbl.hide()
            self._unset_lbl.show()

    def mousePressEvent(self, event) -> None:
        if event.button() == Qt.LeftButton:
            self.clicked.emit()
        super().mousePressEvent(event)


class FolderBar(QFrame):
    """
    Two-slot bar: [📂 Working: path]  [📁 Library: path]  [▲]

    The chevron (▲/▼) on the right collapses/expands the bar in place.

    Signals
    -------
    working_folder_requested()
        User clicked the working folder slot.
    library_folder_changed(Path)
        User picked a library folder.
    collapsed_changed(bool)
        Collapsed state changed — caller can persist this.
    """

    working_folder_requested = Signal()
    library_folder_changed   = Signal(object)   # Path
    collapsed_changed        = Signal(bool)

    def __init__(self, parent=None) -> None:
        super().__init__(parent)
        self.setObjectName("FolderBar")
        self.setFixedHeight(_HEIGHT_EXPANDED)
        self.setStyleSheet(
            "QFrame#FolderBar {"
            "  background: #14142a;"
            "  border-bottom: 1px solid rgba(255,109,0,0.15);"
            "}"
        )

        self._collapsed = False

        outer = QHBoxLayout(self)
        outer.setContentsMargins(8, 4, 4, 4)
        outer.setSpacing(8)

        # ── content: the two folder slots (hidden when collapsed) ──── #
        self._content = QWidget()
        self._content.setStyleSheet("background: transparent;")
        content_lay = QHBoxLayout(self._content)
        content_lay.setContentsMargins(0, 0, 0, 0)
        content_lay.setSpacing(8)

        self._working = _FolderSlot("📂", "WORKING")
        self._library = _FolderSlot("📁", "LIBRARY")

        div = QFrame()
        div.setFrameShape(QFrame.VLine)
        div.setFixedWidth(1)
        div.setStyleSheet("background: rgba(255,109,0,0.12); border: none;")

        content_lay.addWidget(self._working, 1)
        content_lay.addWidget(div)
        content_lay.addWidget(self._library, 1)

        # ── collapsed label (visible only when collapsed) ─────────── #
        self._collapsed_label = QLabel("📂  Folders")
        self._collapsed_label.setStyleSheet(
            "font-size: 11px; font-weight: 600; color: #505070;"
            " background: transparent; letter-spacing: 0.3px;"
        )
        self._collapsed_label.hide()

        # ── chevron button ────────────────────────────────────────── #
        self._chevron = QPushButton("▲")
        self._chevron.setFixedSize(22, 22)
        self._chevron.setCursor(Qt.PointingHandCursor)
        self._chevron.setToolTip("Collapse folder bar")
        self._chevron.setStyleSheet(
            "QPushButton {"
            "  background: transparent;"
            "  color: #404060;"
            "  border: none;"
            "  font-size: 9px;"
            "  border-radius: 4px;"
            "}"
            "QPushButton:hover {"
            "  background: rgba(255,109,0,0.10);"
            "  color: rgba(255,109,0,0.80);"
            "}"
        )

        outer.addWidget(self._content, 1)
        outer.addWidget(self._collapsed_label, 0, Qt.AlignVCenter)
        outer.addStretch(1)
        outer.addWidget(self._chevron, 0, Qt.AlignVCenter)

        self._working.clicked.connect(self.working_folder_requested)
        self._library.clicked.connect(self._pick_library)
        self._chevron.clicked.connect(self.toggle_collapsed)

    # ------------------------------------------------------------------ #
    # Public API                                                           #
    # ------------------------------------------------------------------ #

    def set_working_folder(self, path: Optional[Path]) -> None:
        self._working.set_path(path)

    def set_library_folder(self, path: Optional[Path]) -> None:
        self._library.set_path(path)

    def library_folder(self) -> Optional[Path]:
        return self._library.path()

    def working_folder(self) -> Optional[Path]:
        return self._working.path()

    def is_collapsed(self) -> bool:
        return self._collapsed

    def set_collapsed(self, collapsed: bool) -> None:
        """Set collapsed state without emitting collapsed_changed."""
        self._collapsed = collapsed
        self._apply_collapsed()

    def toggle_collapsed(self) -> None:
        self._collapsed = not self._collapsed
        self._apply_collapsed()
        self.collapsed_changed.emit(self._collapsed)

    # ------------------------------------------------------------------ #
    # Internal                                                             #
    # ------------------------------------------------------------------ #

    def _apply_collapsed(self) -> None:
        if self._collapsed:
            self._content.hide()
            self._collapsed_label.show()
            self.setFixedHeight(_HEIGHT_COLLAPSED)
            self._chevron.setText("▼")
            self._chevron.setToolTip("Show folder bar  (Ctrl+B)")
        else:
            self._content.show()
            self._collapsed_label.hide()
            self.setFixedHeight(_HEIGHT_EXPANDED)
            self._chevron.setText("▲")
            self._chevron.setToolTip("Collapse folder bar  (Ctrl+B)")

    def mousePressEvent(self, event) -> None:
        """Clicking anywhere on the slim collapsed strip expands it."""
        if self._collapsed and event.button() == Qt.LeftButton:
            self.toggle_collapsed()
        super().mousePressEvent(event)

    def _pick_library(self) -> None:
        start = str(self._library.path() or QDir.homePath())
        folder = QFileDialog.getExistingDirectory(
            self,
            "Set Library Folder",
            start,
            QFileDialog.ShowDirsOnly | QFileDialog.DontResolveSymlinks,
        )
        if folder:
            p = Path(folder)
            self._library.set_path(p)
            self.library_folder_changed.emit(p)
