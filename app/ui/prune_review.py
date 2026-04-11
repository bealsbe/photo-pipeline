"""
PruneReviewDialog — shows all files marked for pruning and lets the user
commit them to the system Trash or unmark everything.
"""
from __future__ import annotations

from typing import List

from PySide6.QtCore import Qt, Signal
from PySide6.QtWidgets import (
    QDialog,
    QHBoxLayout,
    QLabel,
    QListWidget,
    QListWidgetItem,
    QMessageBox,
    QPushButton,
    QVBoxLayout,
)

from app.models.photo_record import PhotoRecord
from app.ops.trash import trash_files


def _fmt_size(n: int) -> str:
    for unit in ("B", "KB", "MB", "GB"):
        if n < 1024:
            return f"{n:.1f}\u202f{unit}"
        n //= 1024
    return f"{n:.1f}\u202fTB"


class PruneReviewDialog(QDialog):
    """
    Modal review screen for pruned files.

    Signals
    -------
    committed(List[PhotoRecord])
        Emitted after a successful trash operation with the list of records
        that were actually moved to Trash.  The caller must remove these
        from the collection and models.

    all_unmarked()
        Emitted if the user clicks "Unmark All".  The caller is responsible
        for clearing is_pruned on all records and refreshing the views.
    """

    committed:   Signal = Signal(object)   # List[PhotoRecord]
    all_unmarked: Signal = Signal()

    def __init__(self, records: List[PhotoRecord], parent=None) -> None:
        super().__init__(parent)
        self.setWindowTitle("Review Pruned Files")
        self.setMinimumSize(560, 420)
        self.setModal(True)

        self._records = list(records)
        self._build_ui()

    # ------------------------------------------------------------------ #
    # Construction                                                         #
    # ------------------------------------------------------------------ #

    def _build_ui(self) -> None:
        root = QVBoxLayout(self)
        root.setSpacing(10)
        root.setContentsMargins(16, 14, 16, 14)

        # ── header ──────────────────────────────────────────────────── #
        n = len(self._records)
        total_bytes = sum(r.file_size for r in self._records)
        header = QLabel(
            f"<b>{n} file{'s' if n != 1 else ''} marked for pruning"
            f"  ({_fmt_size(total_bytes)} total)</b>"
        )
        header.setStyleSheet("font-size:13px;")
        root.addWidget(header)

        # ── list ─────────────────────────────────────────────────────── #
        self._list = QListWidget()
        self._list.setAlternatingRowColors(True)
        self._list.setStyleSheet("font-size:12px;")
        self._populate_list()
        root.addWidget(self._list, 1)

        # ── warning ──────────────────────────────────────────────────── #
        warn = QLabel(
            "\u26a0\ufe0f  Files will be moved to the <b>system Trash</b>."
            "  You can restore them from Trash if needed."
        )
        warn.setStyleSheet("color:#c8a040;font-size:11px;")
        warn.setWordWrap(True)
        root.addWidget(warn)

        # ── buttons ──────────────────────────────────────────────────── #
        btn_row = QHBoxLayout()
        btn_row.setSpacing(8)

        self._btn_commit = QPushButton("Move to Trash")
        self._btn_commit.setStyleSheet(
            "QPushButton{background:#a03030;color:#fff;border:none;"
            "border-radius:4px;padding:5px 16px;font-weight:bold;}"
            "QPushButton:hover{background:#c03030;}"
            "QPushButton:disabled{background:#4a2a2a;color:#666;}"
        )
        self._btn_commit.clicked.connect(self._commit)

        btn_unmark = QPushButton("Unmark All")
        btn_unmark.setStyleSheet(
            "QPushButton{background:#6b3a2e;color:#fff;border:none;"
            "border-radius:4px;padding:5px 16px;}"
            "QPushButton:hover{background:#8a4a3a;}"
        )
        btn_unmark.clicked.connect(self._unmark_all)

        btn_close = QPushButton("Close")
        btn_close.setStyleSheet(
            "QPushButton{background:#3a3a3a;color:#ccc;border:none;"
            "border-radius:4px;padding:5px 16px;}"
            "QPushButton:hover{background:#4a4a4a;}"
        )
        btn_close.clicked.connect(self.reject)

        btn_row.addWidget(self._btn_commit)
        btn_row.addWidget(btn_unmark)
        btn_row.addStretch()
        btn_row.addWidget(btn_close)
        root.addLayout(btn_row)

    def _populate_list(self) -> None:
        self._list.clear()
        raw_records = sorted(
            [r for r in self._records if r.file_type.value == "RAW"],
            key=lambda r: r.filename.lower(),
        )
        jpg_records = sorted(
            [r for r in self._records if r.file_type.value == "JPG"],
            key=lambda r: r.filename.lower(),
        )
        for group_label, group in (("RAW", raw_records), ("JPG", jpg_records)):
            if not group:
                continue
            sep = QListWidgetItem(f"── {group_label} ({len(group)}) ──")
            sep.setFlags(Qt.NoItemFlags)
            sep.setForeground(Qt.gray)
            self._list.addItem(sep)
            for r in group:
                item = QListWidgetItem(
                    f"  {r.filename}   [{_fmt_size(r.file_size)}]   {r.path.parent}"
                )
                self._list.addItem(item)

    # ------------------------------------------------------------------ #
    # Button handlers                                                      #
    # ------------------------------------------------------------------ #

    def _commit(self) -> None:
        n = len(self._records)
        reply = QMessageBox.warning(
            self,
            "Move to Trash",
            f"Move <b>{n} file{'s' if n != 1 else ''}</b> to the system Trash?<br><br>"
            "You can restore them from Trash if you change your mind.",
            QMessageBox.Ok | QMessageBox.Cancel,
            QMessageBox.Cancel,
        )
        if reply != QMessageBox.Ok:
            return

        self._btn_commit.setEnabled(False)
        self._btn_commit.setText("Moving…")

        succeeded, failed = trash_files(self._records)

        if failed:
            details = "\n".join(f"  • {r.filename}: {msg}" for r, msg in failed)
            QMessageBox.warning(
                self,
                "Some Files Could Not Be Trashed",
                f"{len(failed)} file{'s' if len(failed) != 1 else ''} could not be moved:\n\n"
                f"{details}",
            )

        if succeeded:
            self.committed.emit(succeeded)

        self.accept()

    def _unmark_all(self) -> None:
        reply = QMessageBox.question(
            self,
            "Unmark All",
            f"Remove the prune mark from all {len(self._records)} file"
            f"{'s' if len(self._records) != 1 else ''}?",
            QMessageBox.Yes | QMessageBox.No,
            QMessageBox.No,
        )
        if reply == QMessageBox.Yes:
            self.all_unmarked.emit()
            self.accept()
