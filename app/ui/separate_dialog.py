"""
SeparateDialog — preview and execute RAW/JPG separation.

Shows each file's proposed move (src → dest), highlights conflicts, and
lets the user pick a conflict strategy before executing.
"""
from __future__ import annotations

from typing import List, Tuple
from pathlib import Path

from PySide6.QtCore import Qt, Signal
from PySide6.QtWidgets import (
    QButtonGroup,
    QDialog,
    QHBoxLayout,
    QLabel,
    QListWidget,
    QListWidgetItem,
    QMessageBox,
    QPushButton,
    QRadioButton,
    QVBoxLayout,
    QWidget,
)

from app.models.photo_record import PhotoRecord
from app.ops.separate import SeparationPlan


def _fmt_size(n: int) -> str:
    for unit in ("B", "KB", "MB", "GB"):
        if n < 1024:
            return f"{n:.1f}\u202f{unit}"
        n //= 1024
    return f"{n:.1f}\u202fTB"


class SeparateDialog(QDialog):
    """
    Modal dialog for RAW/JPG separation.

    Signals
    -------
    separated(succeeded, failed)
        succeeded : List[Tuple[PhotoRecord, Path]]  — (record, new_path)
        failed    : List[Tuple[PhotoRecord, str]]   — (record, error_msg)
    """

    separated: Signal = Signal(object, object)

    def __init__(self, records: List[PhotoRecord], parent=None) -> None:
        super().__init__(parent)
        self.setWindowTitle("Separate RAW / JPG")
        self.setMinimumSize(640, 480)
        self.setModal(True)

        self._plan = SeparationPlan(records)
        self._build_ui()
        self._refresh_list()

    # ------------------------------------------------------------------ #
    # Construction                                                         #
    # ------------------------------------------------------------------ #

    def _build_ui(self) -> None:
        root = QVBoxLayout(self)
        root.setSpacing(10)
        root.setContentsMargins(16, 14, 16, 14)

        # ── header ──────────────────────────────────────────────────── #
        n_move = sum(
            1 for op in self._plan.ops if op.record.path != op.dest
        )
        n_skip = self._plan.already_in_place_count()
        header_text = (
            f"<b>{n_move} file{'s' if n_move != 1 else ''} will be moved</b>"
            + (f",  {n_skip} already in place" if n_skip else "")
        )
        self._lbl_header = QLabel(header_text)
        self._lbl_header.setStyleSheet("font-size:13px;")
        root.addWidget(self._lbl_header)

        # ── conflict controls (only shown when conflicts exist) ───────── #
        self._conflict_widget = QWidget()
        cw_layout = QVBoxLayout(self._conflict_widget)
        cw_layout.setContentsMargins(0, 0, 0, 0)
        cw_layout.setSpacing(4)

        n_conflicts = self._plan.conflict_count()
        conflict_lbl = QLabel(
            f"\u26a0\ufe0f  <b>{n_conflicts} conflict{'s' if n_conflicts != 1 else ''}</b>"
            f" — a file already exists at the destination:"
        )
        conflict_lbl.setStyleSheet("color:#c8a040;font-size:12px;")
        cw_layout.addWidget(conflict_lbl)

        radio_row = QHBoxLayout()
        self._rb_rename = QRadioButton("Auto-rename conflicts  (e.g. IMG_001_1.cr3)")
        self._rb_skip   = QRadioButton("Skip conflicts  (leave originals in place)")
        self._rb_rename.setChecked(True)
        grp = QButtonGroup(self)
        grp.addButton(self._rb_rename)
        grp.addButton(self._rb_skip)
        self._rb_rename.toggled.connect(self._on_strategy_changed)
        radio_row.addWidget(self._rb_rename)
        radio_row.addWidget(self._rb_skip)
        radio_row.addStretch()
        cw_layout.addLayout(radio_row)

        self._conflict_widget.setVisible(self._plan.has_conflicts())
        root.addWidget(self._conflict_widget)

        # ── file list ─────────────────────────────────────────────────── #
        self._list = QListWidget()
        self._list.setAlternatingRowColors(True)
        self._list.setStyleSheet("font-size:11px;")
        root.addWidget(self._list, 1)

        # ── buttons ──────────────────────────────────────────────────── #
        btn_row = QHBoxLayout()
        btn_row.setSpacing(8)

        self._btn_execute = QPushButton("Separate Files")
        self._btn_execute.setStyleSheet(
            "QPushButton{background:#2e5a8e;color:#fff;border:none;"
            "border-radius:4px;padding:5px 16px;font-weight:bold;}"
            "QPushButton:hover{background:#3a72b0;}"
            "QPushButton:disabled{background:#2a3a4a;color:#555;}"
        )
        self._btn_execute.clicked.connect(self._execute)
        self._btn_execute.setEnabled(n_move > 0)

        btn_cancel = QPushButton("Cancel")
        btn_cancel.setStyleSheet(
            "QPushButton{background:#3a3a3a;color:#ccc;border:none;"
            "border-radius:4px;padding:5px 16px;}"
            "QPushButton:hover{background:#4a4a4a;}"
        )
        btn_cancel.clicked.connect(self.reject)

        btn_row.addWidget(self._btn_execute)
        btn_row.addStretch()
        btn_row.addWidget(btn_cancel)
        root.addLayout(btn_row)

    # ------------------------------------------------------------------ #
    # List population                                                      #
    # ------------------------------------------------------------------ #

    def _refresh_list(self) -> None:
        self._list.clear()
        for op in self._plan.ops:
            src = op.record.path
            if src == op.dest:
                # Already in place
                item = QListWidgetItem(
                    f"  \u2713  {op.record.filename}  \u2014  already in place"
                )
                item.setForeground(Qt.darkGray)
            elif op.skipped:
                item = QListWidgetItem(
                    f"  \u23e9  {op.record.filename}  \u2014  SKIP (conflict)"
                )
                item.setForeground(Qt.gray)
            elif op.conflict:
                item = QListWidgetItem(
                    f"  \u26a0  {op.record.filename}  \u2192  {op.final_dest.name}"
                    f"  (renamed, conflict)"
                )
                item.setForeground(Qt.yellow)
            else:
                dest_rel = op.final_dest.relative_to(src.parent.parent
                           ) if src.parent != op.final_dest.parent else op.final_dest
                item = QListWidgetItem(
                    f"  \u2192  {op.record.filename}  \u27a4  "
                    f"{op.final_dest.parent.name}/{op.final_dest.name}"
                )
            self._list.addItem(item)

    # ------------------------------------------------------------------ #
    # Slots                                                                #
    # ------------------------------------------------------------------ #

    def _on_strategy_changed(self) -> None:
        strategy = "skip" if self._rb_skip.isChecked() else "rename"
        self._plan.set_conflict_strategy(strategy)
        self._refresh_list()
        # Update execute button — if all are skipped, nothing to do
        n_actual = sum(
            1 for op in self._plan.ops
            if op.record.path != op.dest and not op.skipped
        )
        self._btn_execute.setEnabled(n_actual > 0)

    def _execute(self) -> None:
        n_move = sum(
            1 for op in self._plan.ops
            if op.record.path != op.dest and not op.skipped
        )
        reply = QMessageBox.question(
            self,
            "Separate Files",
            f"Move <b>{n_move} file{'s' if n_move != 1 else ''}</b> into "
            f"<b>RAW/</b> and <b>JPG/</b> subfolders?<br><br>"
            "This cannot be undone automatically.",
            QMessageBox.Ok | QMessageBox.Cancel,
            QMessageBox.Cancel,
        )
        if reply != QMessageBox.Ok:
            return

        self._btn_execute.setEnabled(False)
        self._btn_execute.setText("Moving…")

        succeeded, failed = self._plan.execute()

        if failed:
            details = "\n".join(f"  • {r.filename}: {msg}" for r, msg in failed)
            QMessageBox.warning(
                self,
                "Some Files Could Not Be Moved",
                f"{len(failed)} file{'s' if len(failed) != 1 else ''} failed:\n\n{details}",
            )

        self.separated.emit(succeeded, failed)
        self.accept()
