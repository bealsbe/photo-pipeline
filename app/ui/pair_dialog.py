"""
PairDialog — preview and save RAW/JPG stem-matched pairs.

Shows each detected pair and lets the user persist them to the sidecar so
they survive across sessions.  Pairing is independent of folder structure
(Sort is a separate operation).
"""
from __future__ import annotations

from collections import defaultdict
from pathlib import Path
from typing import List, Set, Tuple

from PySide6.QtCore import Qt, Signal
from PySide6.QtGui import QColor
from PySide6.QtWidgets import (
    QDialog,
    QHBoxLayout,
    QLabel,
    QListWidget,
    QListWidgetItem,
    QPushButton,
    QVBoxLayout,
)

from app.models.photo_record import FileType, PhotoRecord


# ── palette ────────────────────────────────────────────────────────────────── #
_BG_PAIR   = QColor(255, 109, 0,  18)
_BG_PAIR2  = QColor(255, 109, 0,  10)
_FG_PAIR   = QColor(0xe0, 0xc0, 0x80)
_FG_SINGLE = QColor(0x70, 0x70, 0x90)


def _chip(text: str, bg: str, fg: str = "#c8c8d8") -> QLabel:
    lbl = QLabel(text)
    lbl.setStyleSheet(
        f"background:{bg}; color:{fg};"
        " border-radius:3px; padding:2px 9px;"
        " font-size:11px; font-weight:600;"
    )
    return lbl


class PairDialog(QDialog):
    """
    Modal dialog for confirming and persisting RAW+JPG stem-matched pairs.

    Signals
    -------
    pairs_saved(pair_keys)
        pair_keys : Set[Tuple[str, str]]  — (canonical_parent_abs_str, stem)
        Emitted when the user confirms saving the pairs.
    """

    pairs_saved: Signal = Signal(object)

    def __init__(self, records: List[PhotoRecord], parent=None) -> None:
        super().__init__(parent)
        self.setWindowTitle("Pair RAW + JPG Files")
        self.setMinimumSize(600, 460)
        self.setModal(True)
        self.setStyleSheet("background:#0e0e1a; color:#c8c8d8;")

        self._records = records
        self._groups  = self._detect_pairs()
        self._build_ui()
        self._populate_list()

    # ------------------------------------------------------------------ #
    # Detection                                                            #
    # ------------------------------------------------------------------ #

    @staticmethod
    def _canonical_parent(path: Path) -> Path:
        if path.parent.name.upper() in ("RAW", "JPG"):
            return path.parent.parent
        return path.parent

    def _detect_pairs(self):
        """
        Return a list of groups.  Each group is a list of records that share
        (canonical_parent, lowercase_stem).  Only groups with both RAW and JPG
        are included; single-type groups are returned in an 'unpaired' bucket.
        """
        index: dict = defaultdict(list)
        for r in self._records:
            key = (self._canonical_parent(r.path), r.stem.lower())
            index[key].append(r)

        pairs   = []
        singles = []
        for records in index.values():
            types = {r.file_type for r in records}
            if FileType.RAW in types and FileType.JPG in types:
                # RAW first within each pair group
                records.sort(key=lambda r: (0 if r.file_type == FileType.RAW else 1))
                pairs.append(records)
            else:
                singles.extend(records)

        pairs.sort(key=lambda g: g[0].stem.lower())
        singles.sort(key=lambda r: r.filename.lower())
        return {"pairs": pairs, "singles": singles}

    def pair_keys(self) -> Set[Tuple[str, str]]:
        """Return (canonical_parent_abs_str, stem) for all detected pairs."""
        keys: Set[Tuple[str, str]] = set()
        for group in self._groups["pairs"]:
            r = group[0]
            keys.add((str(self._canonical_parent(r.path)), r.stem.lower()))
        return keys

    # ------------------------------------------------------------------ #
    # UI construction                                                      #
    # ------------------------------------------------------------------ #

    def _build_ui(self) -> None:
        root = QVBoxLayout(self)
        root.setSpacing(10)
        root.setContentsMargins(16, 14, 16, 14)

        n_pairs   = len(self._groups["pairs"])
        n_singles = len(self._groups["singles"])

        # ── header row ────────────────────────────────────────────────── #
        hdr_row = QHBoxLayout()
        hdr_row.setSpacing(8)
        title = QLabel(f"<b>Found {n_pairs} pair{'s' if n_pairs != 1 else ''}</b>")
        title.setStyleSheet("font-size:13px; color:#e0e0f0;")
        hdr_row.addWidget(title)
        if n_pairs:
            hdr_row.addWidget(
                _chip(f"⇄  {n_pairs} RAW+JPG pair{'s' if n_pairs != 1 else ''}",
                      "rgba(255,109,0,0.14)", "#ffaa55")
            )
        if n_singles:
            hdr_row.addWidget(
                _chip(f"{n_singles} unpaired", "rgba(255,255,255,0.06)", "#7070a0")
            )
        hdr_row.addStretch()
        root.addLayout(hdr_row)

        # ── sub-label ─────────────────────────────────────────────────── #
        if n_pairs:
            sub = QLabel(
                "Pairs are matched by filename stem.  "
                "Confirmed pairs are saved to a sidecar file and persist across sessions."
            )
        else:
            sub = QLabel(
                "No RAW+JPG pairs found by stem matching in the current folder."
            )
        sub.setStyleSheet("font-size:11px; color:#6868a0; padding:2px 0 4px 0;")
        sub.setWordWrap(True)
        root.addWidget(sub)

        # ── column header ─────────────────────────────────────────────── #
        col_hdr_row = QHBoxLayout()
        col_hdr_row.setContentsMargins(8, 0, 8, 0)
        _ch_style = "color:#404060; font-size:10px; font-weight:600;"
        lbl_pairs = QLabel("STEM-MATCHED PAIRS")
        lbl_pairs.setStyleSheet(_ch_style)
        lbl_unpaired = QLabel("UNPAIRED")
        lbl_unpaired.setStyleSheet(_ch_style)
        col_hdr_row.addWidget(lbl_pairs, 3)
        col_hdr_row.addWidget(lbl_unpaired, 1)
        root.addLayout(col_hdr_row)

        # ── list ──────────────────────────────────────────────────────── #
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

        # ── buttons ───────────────────────────────────────────────────── #
        btn_row = QHBoxLayout()
        btn_row.setSpacing(8)

        label = (
            f"Save {n_pairs} pair{'s' if n_pairs != 1 else ''}"
            if n_pairs else "No pairs to save"
        )
        self._btn_save = QPushButton(label)
        self._btn_save.setEnabled(n_pairs > 0)
        self._btn_save.setStyleSheet(
            "QPushButton{background:#2e5a8e;color:#fff;border:none;"
            "border-radius:4px;padding:6px 18px;font-weight:bold;font-size:12px;}"
            "QPushButton:hover{background:#3a72b0;}"
            "QPushButton:disabled{background:#1e2a3a;color:#3a4a5a;}"
        )
        self._btn_save.clicked.connect(self._save)

        btn_cancel = QPushButton("Cancel")
        btn_cancel.setStyleSheet(
            "QPushButton{background:rgba(255,255,255,0.06);color:#9090b0;"
            "border:1px solid rgba(255,255,255,0.10);border-radius:4px;"
            "padding:6px 18px;}"
            "QPushButton:hover{background:rgba(255,255,255,0.10);color:#c0c0d8;}"
        )
        btn_cancel.clicked.connect(self.reject)

        btn_row.addWidget(self._btn_save)
        btn_row.addStretch()
        btn_row.addWidget(btn_cancel)
        root.addLayout(btn_row)

    # ------------------------------------------------------------------ #
    # List population                                                      #
    # ------------------------------------------------------------------ #

    def _populate_list(self) -> None:
        self._list.clear()

        for i, group in enumerate(self._groups["pairs"]):
            for j, r in enumerate(group):
                bracket = "┐" if j == 0 else "┘"
                text = f"  ⇄  {r.filename:<32s}  {bracket} paired"
                item = QListWidgetItem(text)
                item.setForeground(_FG_PAIR)
                bg = _BG_PAIR if i % 2 == 0 else _BG_PAIR2
                item.setBackground(bg)
                self._list.addItem(item)

        if self._groups["singles"]:
            # Separator
            sep = QListWidgetItem("  — unpaired —")
            sep.setForeground(QColor(0x40, 0x40, 0x58))
            sep.setFlags(Qt.NoItemFlags)
            self._list.addItem(sep)
            for r in self._groups["singles"]:
                text = f"  ·  {r.filename}"
                item = QListWidgetItem(text)
                item.setForeground(_FG_SINGLE)
                self._list.addItem(item)

    # ------------------------------------------------------------------ #
    # Slots                                                                #
    # ------------------------------------------------------------------ #

    def _save(self) -> None:
        self._btn_save.setEnabled(False)
        self._btn_save.setText("Saving…")
        self.pairs_saved.emit(self.pair_keys())
        self.accept()
