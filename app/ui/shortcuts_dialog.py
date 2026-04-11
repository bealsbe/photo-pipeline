"""
KeyboardShortcutsDialog — reference card for all keyboard shortcuts.
Opened via Help menu or the ? key.
"""
from __future__ import annotations

from PySide6.QtCore import Qt
from PySide6.QtWidgets import (
    QDialog,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QVBoxLayout,
    QWidget,
)


# (key_label, description)  — None inserts a section heading
_SECTIONS = [
    ("Main Window", [
        ("Ctrl+I",          "Import folder (auto-separate RAW/JPG)"),
        ("Ctrl+O",          "Open existing working directory"),
        ("Ctrl+R",          "Review pruned files"),
        ("Ctrl+Shift+S",    "Separate RAW / JPG into subfolders"),
        ("Ctrl+1",          "Switch to List view"),
        ("Ctrl+2",          "Switch to Grid view"),
        ("P  /  Delete",    "Toggle prune mark on selected files"),
        ("?",               "Show this shortcuts reference"),
    ]),
    ("Image Viewer", [
        ("← / →  (or A / D)", "Previous / next image"),
        ("Home",               "First image"),
        ("End",                "Last image"),
        ("P  /  Delete",       "Toggle prune mark"),
        ("U",                  "Unmark (clear prune)"),
        ("F  /  Space",        "Fit image to window"),
        ("1",                  "Actual size (100 %)"),
        ("+  /  =",            "Zoom in"),
        ("−",                  "Zoom out"),
        ("Scroll wheel",       "Zoom in / out"),
        ("Double-click",       "Toggle fit ↔ 100 %"),
        ("Escape",             "Close viewer"),
    ]),
]


class KeyboardShortcutsDialog(QDialog):
    def __init__(self, parent=None) -> None:
        super().__init__(parent)
        self.setWindowTitle("Keyboard Shortcuts")
        self.setMinimumWidth(480)
        self.setModal(False)   # non-modal: user can keep it open while working
        self._build_ui()

    def _build_ui(self) -> None:
        root = QVBoxLayout(self)
        root.setSpacing(0)
        root.setContentsMargins(20, 16, 20, 14)

        title = QLabel("Keyboard Shortcuts")
        title.setStyleSheet("font-size:15px;font-weight:bold;margin-bottom:10px;")
        root.addWidget(title)

        for section_name, rows in _SECTIONS:
            # Section header
            sec_lbl = QLabel(section_name)
            sec_lbl.setStyleSheet(
                "font-size:11px;font-weight:bold;color:#888;"
                "text-transform:uppercase;letter-spacing:1px;"
                "margin-top:12px;margin-bottom:4px;"
            )
            root.addWidget(sec_lbl)

            # Rows
            for key, desc in rows:
                row = QWidget()
                rl = QHBoxLayout(row)
                rl.setContentsMargins(4, 1, 4, 1)
                rl.setSpacing(0)

                key_lbl = QLabel(key)
                key_lbl.setFixedWidth(170)
                key_lbl.setStyleSheet(
                    "font-family:monospace;font-size:12px;"
                    "color:#c8c8ff;"
                    "background:#252540;border-radius:3px;"
                    "padding:2px 6px;"
                )
                key_lbl.setAlignment(Qt.AlignLeft | Qt.AlignVCenter)

                desc_lbl = QLabel(desc)
                desc_lbl.setStyleSheet("font-size:12px;color:#ccc;padding-left:10px;")

                rl.addWidget(key_lbl)
                rl.addWidget(desc_lbl, 1)
                root.addWidget(row)

        root.addSpacing(12)

        btn_row = QHBoxLayout()
        btn_row.addStretch()
        close_btn = QPushButton("Close")
        close_btn.setFixedWidth(80)
        close_btn.setStyleSheet(
            "QPushButton{background:#3a3a3a;color:#ccc;border:none;"
            "border-radius:4px;padding:5px 12px;}"
            "QPushButton:hover{background:#4a4a4a;}"
        )
        close_btn.clicked.connect(self.accept)
        btn_row.addWidget(close_btn)
        root.addLayout(btn_row)
