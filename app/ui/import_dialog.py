"""
ImportDialog — choose a working directory for an import session.

The chosen folder is scanned for all RAW and JPG files, which are then
automatically separated into RAW/ and JPG/ subfolders within that directory.
Paired files (same stem, different extension) remain linked, and pruning
one file in a pair automatically marks its partner too.
"""
from __future__ import annotations

from pathlib import Path
from typing import Optional

from PySide6.QtCore import Qt
from PySide6.QtWidgets import (
    QCheckBox,
    QDialog,
    QFileDialog,
    QFrame,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QPushButton,
    QVBoxLayout,
)


class ImportDialog(QDialog):
    """
    Modal dialog for starting an import session.

    After exec(), check accepted() and call chosen_path() / recursive().
    """

    def __init__(self, parent=None) -> None:
        super().__init__(parent)
        self.setWindowTitle("Import Photos")
        self.setMinimumWidth(480)
        self.setModal(True)
        self._path: Optional[Path] = None
        self._build_ui()

    # ------------------------------------------------------------------ #
    # Public                                                               #
    # ------------------------------------------------------------------ #

    def chosen_path(self) -> Optional[Path]:
        return self._path

    def recursive(self) -> bool:
        return self._recursive_cb.isChecked()

    # ------------------------------------------------------------------ #
    # Construction                                                         #
    # ------------------------------------------------------------------ #

    def _build_ui(self) -> None:
        root = QVBoxLayout(self)
        root.setSpacing(14)
        root.setContentsMargins(20, 18, 20, 16)

        # ── title ──────────────────────────────────────────────────── #
        title = QLabel("Import Photos")
        title.setStyleSheet("font-size:16px;font-weight:bold;")
        root.addWidget(title)

        # ── description ──────────────────────────────────────────── #
        desc = QLabel(
            "Choose the folder that contains your photos. "
            "RAW and JPG files will be automatically sorted into "
            "<b>RAW/</b> and <b>JPG/</b> subfolders. "
            "Paired RAW+JPG files will stay linked, and pruning "
            "one file in a pair marks its partner automatically."
        )
        desc.setWordWrap(True)
        desc.setStyleSheet("color:#aaa;font-size:12px;")
        root.addWidget(desc)

        # ── divider ─────────────────────────────────────────────── #
        line = QFrame()
        line.setFrameShape(QFrame.HLine)
        line.setStyleSheet("color:#333;")
        root.addWidget(line)

        # ── folder picker ───────────────────────────────────────── #
        folder_lbl = QLabel("Working directory:")
        folder_lbl.setStyleSheet("font-size:12px;")
        root.addWidget(folder_lbl)

        picker_row = QHBoxLayout()
        picker_row.setSpacing(6)
        self._path_edit = QLineEdit()
        self._path_edit.setPlaceholderText("No folder selected…")
        self._path_edit.setReadOnly(True)
        self._path_edit.setStyleSheet(
            "QLineEdit{background:#252525;border:1px solid #3a3a3a;"
            "border-radius:4px;padding:4px 8px;color:#ccc;font-size:12px;}"
        )
        btn_browse = QPushButton("Browse…")
        btn_browse.setFixedWidth(80)
        btn_browse.setStyleSheet(
            "QPushButton{background:#3a3a3a;color:#ccc;border:none;"
            "border-radius:4px;padding:5px 10px;}"
            "QPushButton:hover{background:#4a4a4a;}"
        )
        btn_browse.clicked.connect(self._browse)
        picker_row.addWidget(self._path_edit, 1)
        picker_row.addWidget(btn_browse)
        root.addLayout(picker_row)

        # ── recursive option ────────────────────────────────────── #
        self._recursive_cb = QCheckBox("Scan subfolders recursively")
        self._recursive_cb.setChecked(True)
        self._recursive_cb.setStyleSheet("font-size:12px;")
        root.addWidget(self._recursive_cb)

        # ── buttons ─────────────────────────────────────────────── #
        root.addSpacing(4)
        btn_row = QHBoxLayout()
        btn_row.setSpacing(8)

        btn_cancel = QPushButton("Cancel")
        btn_cancel.setStyleSheet(
            "QPushButton{background:#3a3a3a;color:#ccc;border:none;"
            "border-radius:4px;padding:6px 18px;}"
            "QPushButton:hover{background:#4a4a4a;}"
        )
        btn_cancel.clicked.connect(self.reject)

        self._btn_import = QPushButton("Import  →")
        self._btn_import.setEnabled(False)
        self._btn_import.setStyleSheet(
            "QPushButton{background:#2e5a8e;color:#fff;border:none;"
            "border-radius:4px;padding:6px 18px;font-weight:bold;}"
            "QPushButton:hover{background:#3a72b0;}"
            "QPushButton:disabled{background:#2a3a4a;color:#555;}"
        )
        self._btn_import.clicked.connect(self.accept)

        btn_row.addWidget(btn_cancel)
        btn_row.addStretch()
        btn_row.addWidget(self._btn_import)
        root.addLayout(btn_row)

    # ------------------------------------------------------------------ #
    # Internal                                                             #
    # ------------------------------------------------------------------ #

    def _browse(self) -> None:
        folder = QFileDialog.getExistingDirectory(
            self,
            "Choose Working Directory",
            str(self._path) if self._path else "",
            QFileDialog.ShowDirsOnly | QFileDialog.DontResolveSymlinks,
        )
        if folder:
            self._path = Path(folder)
            self._path_edit.setText(folder)
            self._btn_import.setEnabled(True)
