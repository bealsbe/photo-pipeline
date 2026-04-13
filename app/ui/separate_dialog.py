"""
SeparateDialog — preview and execute RAW/JPG separation.

Shows each file's proposed move grouped so paired files sit side-by-side
with a visual link indicator, making it clear they will remain a RAW+JPG
pair after separation.  Conflict strategy and execution are unchanged.
"""
from __future__ import annotations

from collections import defaultdict
from pathlib import Path
from typing import List, Tuple

from PySide6.QtCore import Qt, Signal
from PySide6.QtGui import QColor
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

from app.models.photo_record import FileType, PhotoRecord
from app.ops.separate import SeparationPlan, MoveOp


# ── palette ────────────────────────────────────────────────────────────────── #
_BG_PAIR    = QColor(255, 109, 0,  18)   # faint orange tint for paired rows
_BG_PAIR2   = QColor(255, 109, 0,  10)   # slightly lighter for second row of pair
_FG_PAIR    = QColor(0xe0, 0xc0, 0x80)   # warm cream for paired filenames
_FG_SINGLE  = QColor(0x90, 0x90, 0xb0)   # muted for unpaired filenames
_FG_INPLACE = QColor(0x55, 0x55, 0x70)   # dim for already-in-place
_FG_SKIP    = QColor(0x50, 0x50, 0x60)   # dimmer for skipped
_FG_WARN    = QColor(0xc8, 0xa0, 0x40)   # yellow-amber for conflict/renamed
_FG_LINK    = QColor(0xff, 0x6d, 0x00)   # orange for link badge


def _fmt_size(n: int) -> str:
    for unit in ("B", "KB", "MB", "GB"):
        if n < 1024:
            return f"{n:.1f}\u202f{unit}"
        n //= 1024
    return f"{n:.1f}\u202fTB"


def _chip(text: str, bg: str, fg: str = "#c8c8d8") -> QLabel:
    """Small rounded-rectangle stat chip."""
    lbl = QLabel(text)
    lbl.setStyleSheet(
        f"background:{bg}; color:{fg};"
        " border-radius:3px; padding:2px 9px;"
        " font-size:11px; font-weight:600;"
    )
    return lbl


class SeparateDialog(QDialog):
    """
    Modal dialog for RAW/JPG separation.

    Paired files are shown side-by-side with a link badge so the user
    can see that after separation the RAW and JPG will remain connected.

    Signals
    -------
    separated(succeeded, failed)
        succeeded : List[Tuple[PhotoRecord, Path]]  — (record, new_path)
        failed    : List[Tuple[PhotoRecord, str]]   — (record, error_msg)
    """

    separated: Signal = Signal(object, object)

    def __init__(self, records: List[PhotoRecord], parent=None) -> None:
        super().__init__(parent)
        self.setWindowTitle("Sort Files")
        self.setMinimumSize(680, 520)
        self.setModal(True)
        self.setStyleSheet("background:#0e0e1a; color:#c8c8d8;")

        self._plan = SeparationPlan(records)
        self._build_ui()
        self._refresh_list()

    # ------------------------------------------------------------------ #
    # Internal helpers                                                      #
    # ------------------------------------------------------------------ #

    @staticmethod
    def _canonical_parent(path: Path) -> Path:
        if path.parent.name.upper() in ("RAW", "JPG"):
            return path.parent.parent
        return path.parent

    def _stem_group_count(self) -> int:
        """Number of groups where both a RAW and a JPG share the same stem."""
        groups: dict = defaultdict(set)
        for op in self._plan.ops:
            r = op.record
            key = (self._canonical_parent(r.path), r.stem.lower())
            groups[key].add(r.file_type)
        return sum(
            1 for types in groups.values()
            if FileType.RAW in types and FileType.JPG in types
        )

    def _group_ops(self):
        """
        Return ops in display order:
          1. Stem groups — RAW and JPG with the same stem, sorted by stem
          2. Singles — files with no same-stem partner, sorted by filename
        Groups with both types are shown adjacent so the user can see which
        files will land in sibling RAW/ and JPG/ folders after sorting.
        """
        stem_groups: dict = defaultdict(list)
        singles: list = []

        for op in self._plan.ops:
            r = op.record
            key = (self._canonical_parent(r.path), r.stem.lower())
            stem_groups[key].append(op)

        result = []
        for key, group in sorted(stem_groups.items(), key=lambda kv: kv[0][1]):
            types = {op.record.file_type for op in group}
            if FileType.RAW in types and FileType.JPG in types:
                group.sort(key=lambda op: (0 if op.record.file_type == FileType.RAW else 1))
                result.append(group)
            else:
                singles.extend(group)

        singles.sort(key=lambda op: op.record.filename.lower())
        for op in singles:
            result.append([op])
        return result

    # ------------------------------------------------------------------ #
    # Construction                                                          #
    # ------------------------------------------------------------------ #

    def _build_ui(self) -> None:
        root = QVBoxLayout(self)
        root.setSpacing(10)
        root.setContentsMargins(16, 14, 16, 14)

        n_move        = sum(1 for op in self._plan.ops if op.record.path != op.dest)
        n_skip        = self._plan.already_in_place_count()
        n_stem_groups = self._stem_group_count()

        # ── stat chips row ─────────────────────────────────────────────── #
        chips_row = QHBoxLayout()
        chips_row.setSpacing(8)

        move_lbl = QLabel(
            f"<b>Sort {n_move} file{'s' if n_move != 1 else ''}</b>"
        )
        move_lbl.setStyleSheet("font-size:13px; color:#e0e0f0;")
        chips_row.addWidget(move_lbl)

        if n_skip:
            chips_row.addWidget(
                _chip(f"{n_skip} already sorted",
                      "rgba(255,255,255,0.06)", "#7070a0")
            )
        if n_stem_groups:
            chips_row.addWidget(
                _chip(f"⇄  {n_stem_groups} stem group{'s' if n_stem_groups != 1 else ''}",
                      "rgba(255,109,0,0.14)", "#ffaa55")
            )

        chips_row.addStretch()
        root.addLayout(chips_row)

        # ── explanatory sub-label ───────────────────────────────────────── #
        sub_parts = []
        if n_move:
            sub_parts.append(
                "Files will be moved into <b>RAW/</b> and <b>JPG/</b> subfolders."
            )
        if n_stem_groups:
            sub_parts.append(
                f"{n_stem_groups} RAW+JPG stem group{'s' if n_stem_groups != 1 else ''} "
                "highlighted in orange — these share a filename stem."
            )
        if sub_parts:
            sub = QLabel("  ".join(sub_parts))
            sub.setStyleSheet(
                "font-size:11px; color:#6868a0;"
                " padding: 2px 0 4px 0;"
            )
            sub.setWordWrap(True)
            root.addWidget(sub)

        # ── conflict controls ───────────────────────────────────────────── #
        self._conflict_widget = QWidget()
        cw_layout = QVBoxLayout(self._conflict_widget)
        cw_layout.setContentsMargins(0, 0, 0, 0)
        cw_layout.setSpacing(4)

        n_conflicts = self._plan.conflict_count()
        conflict_lbl = QLabel(
            f"\u26a0\ufe0f  <b>{n_conflicts} conflict{'s' if n_conflicts != 1 else ''}</b>"
            " — a file already exists at the destination:"
        )
        conflict_lbl.setStyleSheet("color:#c8a040; font-size:12px;")
        cw_layout.addWidget(conflict_lbl)

        radio_row = QHBoxLayout()
        self._rb_rename = QRadioButton("Auto-rename  (e.g. IMG_001_1.cr3)")
        self._rb_skip   = QRadioButton("Skip conflicting files")
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

        # ── column header ───────────────────────────────────────────────── #
        col_hdr = QWidget()
        col_hdr.setFixedHeight(20)
        col_hdr.setStyleSheet("background: transparent;")
        col_lay = QHBoxLayout(col_hdr)
        col_lay.setContentsMargins(8, 0, 8, 0)
        col_lay.setSpacing(0)
        def _hdr(text, stretch=1):
            lbl = QLabel(text)
            lbl.setStyleSheet("color:#404060; font-size:10px; font-weight:600;")
            col_lay.addWidget(lbl, stretch)
        _hdr("FILE", 3)
        _hdr("DESTINATION", 3)
        _hdr("STATUS", 2)
        root.addWidget(col_hdr)

        # ── file list ──────────────────────────────────────────────────── #
        self._list = QListWidget()
        self._list.setAlternatingRowColors(False)
        self._list.setStyleSheet(
            "QListWidget {"
            "  background: #0a0a12;"
            "  border: 1px solid rgba(255,109,0,0.12);"
            "  border-radius: 4px;"
            "  font-family: monospace; font-size: 11px;"
            "  outline: none;"
            "}"
            "QListWidget::item { padding: 2px 8px; }"
            "QListWidget::item:selected { background: rgba(255,109,0,0.12); }"
        )
        root.addWidget(self._list, 1)

        # ── buttons ──────────────────────────────────────────────────── #
        btn_row = QHBoxLayout()
        btn_row.setSpacing(8)

        self._btn_execute = QPushButton("Sort Files")
        self._btn_execute.setStyleSheet(
            "QPushButton{background:#2e5a8e;color:#fff;border:none;"
            "border-radius:4px;padding:6px 18px;font-weight:bold;font-size:12px;}"
            "QPushButton:hover{background:#3a72b0;}"
            "QPushButton:disabled{background:#1e2a3a;color:#3a4a5a;}"
        )
        self._btn_execute.clicked.connect(self._execute)
        self._btn_execute.setEnabled(n_move > 0)

        btn_cancel = QPushButton("Cancel")
        btn_cancel.setStyleSheet(
            "QPushButton{background:rgba(255,255,255,0.06);color:#9090b0;"
            "border:1px solid rgba(255,255,255,0.10);border-radius:4px;"
            "padding:6px 18px;}"
            "QPushButton:hover{background:rgba(255,255,255,0.10);color:#c0c0d8;}"
        )
        btn_cancel.clicked.connect(self.reject)

        btn_row.addWidget(self._btn_execute)
        btn_row.addStretch()
        btn_row.addWidget(btn_cancel)
        root.addLayout(btn_row)

    # ------------------------------------------------------------------ #
    # List population                                                       #
    # ------------------------------------------------------------------ #

    def _refresh_list(self) -> None:
        self._list.clear()
        groups = self._group_ops()

        for group in groups:
            is_pair = len(group) == 2
            for i, op in enumerate(group):
                item = self._make_item(op, is_pair=is_pair, pair_index=i, pair_size=len(group))
                self._list.addItem(item)

    def _make_item(self, op: MoveOp, *, is_pair: bool, pair_index: int, pair_size: int) -> QListWidgetItem:
        src = op.record.path

        if src == op.dest:
            text = f"  ✓  {op.record.filename:<30s}  already in place"
            item = QListWidgetItem(text)
            item.setForeground(_FG_INPLACE)
            return item

        dest_label = f"{op.final_dest.parent.name}/{op.final_dest.name}"

        if op.skipped:
            text = f"  ⏭  {op.record.filename:<30s}  →  {dest_label}   SKIP"
            item = QListWidgetItem(text)
            item.setForeground(_FG_SKIP)
            return item

        if is_pair:
            # Same-stem group — bracket shows they share a filename stem
            if op.conflict:
                status = "⚠  renamed"
                fg = _FG_WARN
            else:
                if pair_size == 2:
                    bracket = "┐" if pair_index == 0 else "┘"
                else:
                    bracket = "┐" if pair_index == 0 else ("┘" if pair_index == pair_size - 1 else "│")
                status = f"{bracket} same stem"
                fg = _FG_PAIR

            text = f"  ⇄  {op.record.filename:<30s}  →  {dest_label:<28s}  {status}"
            item = QListWidgetItem(text)
            item.setForeground(fg)
            bg = _BG_PAIR if pair_index % 2 == 0 else _BG_PAIR2
            item.setBackground(bg)
        else:
            if op.conflict:
                text = f"  ⚠  {op.record.filename:<30s}  →  {dest_label:<28s}  renamed"
                item = QListWidgetItem(text)
                item.setForeground(_FG_WARN)
            else:
                text = f"  →  {op.record.filename:<30s}  →  {dest_label}"
                item = QListWidgetItem(text)
                item.setForeground(_FG_SINGLE)

        return item

    # ------------------------------------------------------------------ #
    # Slots                                                                 #
    # ------------------------------------------------------------------ #

    def _on_strategy_changed(self) -> None:
        strategy = "skip" if self._rb_skip.isChecked() else "rename"
        self._plan.set_conflict_strategy(strategy)
        self._refresh_list()
        n_actual = sum(
            1 for op in self._plan.ops
            if op.record.path != op.dest and not op.skipped
        )
        self._btn_execute.setEnabled(n_actual > 0)

    def _execute(self) -> None:
        n_move = sum(1 for op in self._plan.ops if op.record.path != op.dest and not op.skipped)

        reply = QMessageBox.question(
            self,
            "Sort Files",
            f"Move <b>{n_move} file{'s' if n_move != 1 else ''}</b> into "
            "<b>RAW/</b> and <b>JPG/</b> subfolders?"
            "<br><br>"
            "<small>Files can be moved back manually if needed. "
            "Use the <b>Pair</b> button to record RAW+JPG relationships.</small>",
            QMessageBox.Ok | QMessageBox.Cancel,
            QMessageBox.Cancel,
        )
        if reply != QMessageBox.Ok:
            return

        self._btn_execute.setEnabled(False)
        self._btn_execute.setText("Sorting…")

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
