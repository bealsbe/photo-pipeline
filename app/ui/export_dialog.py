"""
ExportDialog — export the current working folder into the standard library structure.

Wizard layout
-------------
  Card 1  — Source & destination paths
  Card 2  — Operation (Copy / Move toggle) + include-pruned checkbox
  Section — Preview tree (year / month / type rows with file counts)
  Footer  — Export button + Cancel

Signals
-------
exported(succeeded, failed)
    Emitted after execution completes.
library_folder_changed(Path)
    Emitted when the user picks a new library folder from inside this dialog.
"""
from __future__ import annotations

from pathlib import Path
from typing import Callable, List, Optional

from PySide6.QtCore import (
    QDir,
    QObject,
    QRunnable,
    Qt,
    QThreadPool,
    Signal,
)
from PySide6.QtWidgets import (
    QCheckBox,
    QDialog,
    QFileDialog,
    QFrame,
    QHBoxLayout,
    QLabel,
    QProgressBar,
    QPushButton,
    QScrollArea,
    QSizePolicy,
    QVBoxLayout,
    QWidget,
)

from app.models.photo_record import FileType, PhotoRecord
from app.ops.library import LibraryPlan, Resolution


# ── colour / style constants ───────────────────────────────────────────────── #

_BG_DIALOG  = "#0c0c18"
_BG_CARD    = "#12121f"
_BG_SECTION = "#0f0f1c"
_BORDER     = "rgba(255,109,0,0.18)"
_BORDER_HI  = "rgba(255,109,0,0.45)"
_ORANGE     = "#ff6d00"
_TEXT_PRI   = "#d0d0e8"
_TEXT_SEC   = "#7878a0"
_TEXT_DIM   = "#3a3a58"

_CARD_QSS = (
    f"background:{_BG_CARD};"
    f" border:1px solid {_BORDER};"
    " border-radius:6px;"
)


def _card(parent=None) -> QWidget:
    w = QWidget(parent)
    w.setStyleSheet(f"QWidget#{w.objectName() or 'card'} {{ {_CARD_QSS} }}")
    w.setObjectName("card")
    w.setStyleSheet(f"#card {{ {_CARD_QSS} }}")
    return w


def _h_rule() -> QFrame:
    f = QFrame()
    f.setFrameShape(QFrame.HLine)
    f.setFixedHeight(1)
    f.setStyleSheet(f"background:{_BORDER}; border:none;")
    return f


def _elide(path: Path, max_len: int = 58) -> str:
    s = str(path)
    return s if len(s) <= max_len else "…" + s[-(max_len - 1):]


# ── execute worker ─────────────────────────────────────────────────────────── #

class _ExecSignals(QObject):
    progress: Signal = Signal(int, int)
    finished: Signal = Signal(object, object)


class _ExecWorker(QRunnable):
    def __init__(self, plan: LibraryPlan, mode: str, signals: _ExecSignals) -> None:
        super().__init__()
        self._plan    = plan
        self._mode    = mode
        self._signals = signals
        self.setAutoDelete(True)

    def run(self) -> None:
        succeeded, failed = self._plan.execute(
            self._mode,
            progress=lambda d, t: self._signals.progress.emit(d, t),
        )
        self._signals.finished.emit(succeeded, failed)


# ── main dialog ────────────────────────────────────────────────────────────── #

class ExportDialog(QDialog):
    exported:               Signal = Signal(object, object)
    library_folder_changed: Signal = Signal(object)

    def __init__(
        self,
        records:        List[PhotoRecord],
        working_folder: Optional[Path],
        library_folder: Optional[Path],
        pair_lookup:    Optional[Callable[[PhotoRecord], Optional[PhotoRecord]]] = None,
        parent=None,
    ) -> None:
        super().__init__(parent)
        self.setWindowTitle("Export to Library")
        self.setMinimumSize(700, 640)
        self.setModal(True)
        self.setStyleSheet(
            f"QDialog {{ background:{_BG_DIALOG}; color:{_TEXT_PRI}; }}"
            f"QLabel   {{ background:transparent; color:{_TEXT_PRI}; }}"
            f"QScrollArea {{ background:{_BG_SECTION}; border:none; }}"
            f"QScrollBar:vertical {{ background:#0a0a14; width:6px; border:none; }}"
            f"QScrollBar::handle:vertical {{ background:#2a2a44; border-radius:3px; }}"
            f"QScrollBar::add-line:vertical, QScrollBar::sub-line:vertical {{ height:0; }}"
        )

        self._all_records    = list(records)
        self._working_folder = working_folder
        self._library_folder = library_folder
        self._pair_lookup    = pair_lookup
        self._plan: Optional[LibraryPlan] = None
        self._bulk_btns: dict[Resolution, QPushButton] = {}

        self._build_ui()
        self._rebuild_plan()

    # ──────────────────────────────────────────────────────────────────────
    # Construction
    # ──────────────────────────────────────────────────────────────────────

    def _build_ui(self) -> None:
        root = QVBoxLayout(self)
        root.setSpacing(12)
        root.setContentsMargins(16, 16, 16, 16)

        # ── Card 1: paths ─────────────────────────────────────────────── #
        paths_card = QWidget()
        paths_card.setObjectName("card")
        paths_card.setStyleSheet(f"#card {{ {_CARD_QSS} }}")
        paths_lay = QVBoxLayout(paths_card)
        paths_lay.setContentsMargins(14, 12, 14, 12)
        paths_lay.setSpacing(8)

        self._working_lbl, w_row = self._make_path_row("📂", "WORKING", self._working_folder)
        self._library_lbl, lib_row, self._btn_change_lib = self._make_path_row(
            "📁", "LIBRARY", self._library_folder, link="change"
        )
        paths_lay.addLayout(w_row)
        paths_lay.addWidget(_h_rule())
        paths_lay.addLayout(lib_row)
        root.addWidget(paths_card)

        # ── Card 2: operation ─────────────────────────────────────────── #
        op_card = QWidget()
        op_card.setObjectName("card")
        op_card.setStyleSheet(f"#card {{ {_CARD_QSS} }}")
        op_lay = QVBoxLayout(op_card)
        op_lay.setContentsMargins(14, 14, 14, 14)
        op_lay.setSpacing(12)

        op_title = QLabel("Operation")
        op_title.setStyleSheet(
            f"font-size:10px; font-weight:700; color:{_TEXT_SEC};"
            " letter-spacing:1px; text-transform:uppercase;"
        )
        op_lay.addWidget(op_title)

        # Copy / Move toggle buttons
        toggle_row = QHBoxLayout()
        toggle_row.setSpacing(8)

        self._btn_copy = self._make_mode_btn("Copy", "Keep originals in working folder")
        self._btn_move = self._make_mode_btn("Move", "Remove originals after export")
        self._btn_copy.setProperty("selected", True)
        self._btn_copy.setStyleSheet(self._mode_btn_qss(selected=True))
        self._btn_move.setStyleSheet(self._mode_btn_qss(selected=False))
        self._btn_copy.clicked.connect(lambda: self._select_mode("copy"))
        self._btn_move.clicked.connect(lambda: self._select_mode("move"))

        toggle_row.addWidget(self._btn_copy)
        toggle_row.addWidget(self._btn_move)
        op_lay.addLayout(toggle_row)

        # Include pruned checkbox
        self._chk_pruned = QCheckBox("Include pruned files in export")
        self._chk_pruned.setChecked(False)
        self._chk_pruned.setStyleSheet(
            f"QCheckBox {{ color:{_TEXT_SEC}; font-size:12px; spacing:8px; }}"
            f"QCheckBox::indicator {{ width:15px; height:15px; border-radius:3px;"
            f" border:1px solid rgba(255,109,0,0.28); background:#0a0a14; }}"
            f"QCheckBox::indicator:hover {{ border-color:rgba(255,109,0,0.55); }}"
            f"QCheckBox::indicator:checked {{ background:rgba(255,109,0,0.35);"
            f" border-color:rgba(255,109,0,0.70); }}"
        )
        op_lay.addWidget(self._chk_pruned)
        root.addWidget(op_card)

        # ── Stats row ─────────────────────────────────────────────────── #
        self._stats_widget = QWidget()
        self._stats_widget.setStyleSheet("background:transparent;")
        self._stats_lay = QHBoxLayout(self._stats_widget)
        self._stats_lay.setContentsMargins(2, 0, 2, 0)
        self._stats_lay.setSpacing(8)
        root.addWidget(self._stats_widget)

        # ── Preview section ───────────────────────────────────────────── #
        self._preview_scroll = QScrollArea()
        self._preview_scroll.setWidgetResizable(True)
        self._preview_scroll.setStyleSheet(
            f"QScrollArea {{ background:{_BG_SECTION};"
            f" border:1px solid {_BORDER}; border-radius:6px; }}"
            f"QScrollBar:vertical {{ background:#0a0a14; width:6px; border:none; }}"
            f"QScrollBar::handle:vertical {{ background:#2a2a44; border-radius:3px; }}"
            f"QScrollBar::add-line:vertical, QScrollBar::sub-line:vertical {{ height:0; }}"
        )
        self._preview_inner = QWidget()
        self._preview_inner.setStyleSheet(f"background:{_BG_SECTION};")
        self._preview_lay = QVBoxLayout(self._preview_inner)
        self._preview_lay.setContentsMargins(14, 10, 14, 10)
        self._preview_lay.setSpacing(3)
        self._preview_scroll.setWidget(self._preview_inner)
        root.addWidget(self._preview_scroll, 1)

        # ── Conflict bar (compact single row) ─────────────────────────── #
        self._conflict_section = QWidget()
        self._conflict_section.setObjectName("cbar")
        self._conflict_section.setStyleSheet(
            "#cbar { background:#18100c; border:1px solid rgba(200,140,0,0.28);"
            " border-radius:6px; }"
        )
        cbar_lay = QHBoxLayout(self._conflict_section)
        cbar_lay.setContentsMargins(14, 10, 14, 10)
        cbar_lay.setSpacing(10)

        self._conflict_hdr_lbl = QLabel()
        self._conflict_hdr_lbl.setStyleSheet("font-size:12px; color:#c8a040; font-weight:600;")
        cbar_lay.addWidget(self._conflict_hdr_lbl)
        cbar_lay.addStretch()

        bulk_lbl = QLabel("Apply to all:")
        bulk_lbl.setStyleSheet(f"font-size:11px; color:{_TEXT_SEC};")
        cbar_lay.addWidget(bulk_lbl)

        _inactive_qss = (
            "QPushButton { background:#1e1a10; color:#a09060;"
            " border:1px solid rgba(200,140,0,0.25); border-radius:3px;"
            " font-size:11px; padding:0 10px; }"
            "QPushButton:hover { background:#2a2010; color:#d0b060;"
            " border-color:rgba(200,140,0,0.55); }"
        )
        _active_qss = (
            "QPushButton { background:rgba(200,140,0,0.25); color:#e0c070;"
            " border:1px solid rgba(200,140,0,0.65); border-radius:3px;"
            " font-size:11px; font-weight:700; padding:0 10px; }"
        )
        for label, res in (("Skip", Resolution.SKIP),
                           ("Overwrite", Resolution.OVERWRITE),
                           ("Rename", Resolution.RENAME)):
            b = QPushButton(label)
            b.setFixedHeight(24)
            b.setStyleSheet(_inactive_qss)
            b.setProperty("_inactive_qss", _inactive_qss)
            b.setProperty("_active_qss", _active_qss)
            b.clicked.connect(lambda _=False, r=res: self._bulk_resolve(r))
            cbar_lay.addWidget(b)
            self._bulk_btns[res] = b

        self._conflict_section.hide()
        root.addWidget(self._conflict_section)

        # ── Progress bar ──────────────────────────────────────────────── #
        self._progress_widget = QWidget()
        self._progress_widget.setStyleSheet("background:transparent;")
        prog_lay = QVBoxLayout(self._progress_widget)
        prog_lay.setContentsMargins(2, 4, 2, 4)
        prog_lay.setSpacing(5)
        self._progress_bar = QProgressBar()
        self._progress_bar.setRange(0, 100)
        self._progress_bar.setValue(0)
        self._progress_bar.setTextVisible(False)
        self._progress_bar.setFixedHeight(6)
        self._progress_bar.setStyleSheet(
            "QProgressBar { background:#1a1a2e; border-radius:3px; border:none; }"
            "QProgressBar::chunk { background:#ff6d00; border-radius:3px; }"
        )
        self._progress_lbl = QLabel("Preparing…")
        self._progress_lbl.setStyleSheet(f"font-size:11px; color:{_TEXT_SEC};")
        prog_lay.addWidget(self._progress_bar)
        prog_lay.addWidget(self._progress_lbl)
        self._progress_widget.hide()
        root.addWidget(self._progress_widget)

        # ── Footer ────────────────────────────────────────────────────── #
        root.addWidget(_h_rule())
        footer = QHBoxLayout()
        footer.setSpacing(10)
        footer.setContentsMargins(0, 4, 0, 0)

        self._btn_export = QPushButton("Export")
        self._btn_export.setFixedHeight(36)
        self._btn_export.setMinimumWidth(170)
        self._btn_export.setCursor(Qt.PointingHandCursor)
        self._btn_export.setStyleSheet(
            "QPushButton { background:#1e4d96; color:#a8d0ff;"
            " border:1px solid rgba(100,170,255,0.40);"
            " border-radius:5px; font-size:13px; font-weight:700; padding:0 24px; }"
            "QPushButton:hover { background:#2560b8; color:#cce4ff;"
            " border-color:rgba(100,170,255,0.70); }"
            "QPushButton:disabled { background:#111128; color:#303050;"
            " border-color:rgba(100,170,255,0.06); }"
        )
        self._btn_export.clicked.connect(self._start_export)

        self._btn_cancel = QPushButton("Cancel")
        self._btn_cancel.setFixedHeight(36)
        self._btn_cancel.setCursor(Qt.PointingHandCursor)
        self._btn_cancel.setStyleSheet(
            f"QPushButton {{ background:transparent; color:{_TEXT_SEC};"
            " border:1px solid rgba(255,255,255,0.10);"
            " border-radius:5px; font-size:12px; padding:0 18px; }"
            "QPushButton:hover { background:rgba(255,255,255,0.07); color:#b0b0cc;"
            " border-color:rgba(255,255,255,0.20); }"
        )
        self._btn_cancel.clicked.connect(self.reject)

        footer.addWidget(self._btn_export)
        footer.addStretch()
        footer.addWidget(self._btn_cancel)
        root.addLayout(footer)

        # ── Wire signals ──────────────────────────────────────────────── #
        self._btn_change_lib.clicked.connect(self._pick_library)
        self._chk_pruned.toggled.connect(lambda _: self._rebuild_plan())

    # ──────────────────────────────────────────────────────────────────────
    # Widget helpers
    # ──────────────────────────────────────────────────────────────────────

    def _make_path_row(self, emoji: str, label: str, path: Optional[Path],
                       link: str = ""):
        """Returns (path_label, layout[, link_btn])."""
        row = QHBoxLayout()
        row.setSpacing(10)

        icon_lbl = QLabel(emoji)
        icon_lbl.setStyleSheet("font-size:15px;")
        icon_lbl.setFixedWidth(22)

        type_lbl = QLabel(label)
        type_lbl.setStyleSheet(
            f"font-size:10px; font-weight:700; color:{_TEXT_DIM};"
            " letter-spacing:1px; min-width:56px;"
        )

        path_text = _elide(path) if path else ("click to set…" if link else "—")
        path_lbl = QLabel(path_text)
        path_lbl.setStyleSheet(
            f"font-size:12px; color:{ _TEXT_SEC if path else '#2e2e4a'};"
            + (" font-style:italic;" if not path and link else "")
        )
        path_lbl.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Fixed)

        row.addWidget(icon_lbl)
        row.addWidget(type_lbl)
        row.addWidget(path_lbl, 1)

        if link:
            btn = QPushButton(link)
            btn.setFlat(True)
            btn.setCursor(Qt.PointingHandCursor)
            btn.setStyleSheet(
                "QPushButton { color:rgba(255,109,0,0.50); font-size:11px;"
                " background:transparent; border:none; padding:0; }"
                "QPushButton:hover { color:#ff6d00; }"
            )
            row.addWidget(btn)
            return path_lbl, row, btn

        return path_lbl, row

    @staticmethod
    def _make_mode_btn(title: str, subtitle: str) -> QPushButton:
        btn = QPushButton()
        btn.setText(f"{title}\n{subtitle}")
        btn.setCheckable(False)
        btn.setFixedHeight(62)
        btn.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Fixed)
        btn.setCursor(Qt.PointingHandCursor)
        return btn

    @staticmethod
    def _mode_btn_qss(selected: bool) -> str:
        if selected:
            return (
                "QPushButton { background:rgba(255,109,0,0.12);"
                " border:1px solid rgba(255,109,0,0.55);"
                " border-radius:5px; color:#e8c898;"
                " font-size:13px; font-weight:700; text-align:center;"
                " padding:6px 12px; }"
                "QPushButton:hover { background:rgba(255,109,0,0.18); }"
            )
        return (
            "QPushButton { background:#0e0e1c;"
            " border:1px solid rgba(255,255,255,0.10);"
            " border-radius:5px; color:#505068;"
            " font-size:13px; font-weight:700; text-align:center;"
            " padding:6px 12px; }"
            "QPushButton:hover { background:#141428; color:#8888a8;"
            " border-color:rgba(255,255,255,0.20); }"
        )

    def _select_mode(self, mode: str) -> None:
        self._btn_copy.setStyleSheet(self._mode_btn_qss(mode == "copy"))
        self._btn_move.setStyleSheet(self._mode_btn_qss(mode == "move"))
        self._update_export_button()

    def _current_mode(self) -> str:
        """Return 'copy' or 'move' based on which button is styled as selected."""
        qss = self._btn_move.styleSheet()
        return "move" if "rgba(255,109,0,0.12)" in qss else "copy"

    # ──────────────────────────────────────────────────────────────────────
    # Plan management
    # ──────────────────────────────────────────────────────────────────────

    def _active_records(self) -> List[PhotoRecord]:
        if self._chk_pruned.isChecked():
            return self._all_records
        return [r for r in self._all_records if not r.is_pruned]

    def _rebuild_plan(self) -> None:
        if not self._library_folder:
            self._plan = None
            self._refresh_stats(0, 0, 0, 0)
            self._refresh_preview([])
            self._refresh_conflict_section()
            self._update_export_button()
            return

        records = self._active_records()
        self._plan = LibraryPlan(records, self._library_folder, self._pair_lookup)

        n_unp_raw = sum(1 for r in records if r.file_type == FileType.RAW and not r.is_paired)
        n_unp_jpg = sum(1 for r in records if r.file_type == FileType.JPG and not r.is_paired)
        n_pruned  = sum(1 for r in records if r.is_pruned) if self._chk_pruned.isChecked() else 0

        self._refresh_stats(self._plan.total, self._plan.conflict_count, n_unp_raw, n_unp_jpg, n_pruned)
        self._refresh_preview(self._plan.grouped())
        self._refresh_conflict_section()
        self._update_export_button()

    def _refresh_stats(self, n_total: int, n_conflict: int,
                       n_unp_raw: int, n_unp_jpg: int,
                       n_pruned: int = 0) -> None:
        while self._stats_lay.count():
            item = self._stats_lay.takeAt(0)
            if item.widget():
                item.widget().deleteLater()

        if n_total == 0 and not self._library_folder:
            return

        def chip(text: str, bg: str, fg: str) -> QLabel:
            l = QLabel(text)
            l.setStyleSheet(
                f"background:{bg}; color:{fg}; border-radius:4px;"
                " padding:3px 10px; font-size:11px; font-weight:600;"
            )
            return l

        mode = self._current_mode()
        self._stats_lay.addWidget(
            chip(f"{n_total} file{'s' if n_total != 1 else ''} to {mode}",
                 "#181830", "#9898c0")
        )
        if n_conflict:
            self._stats_lay.addWidget(
                chip(f"⚠  {n_conflict} conflict{'s' if n_conflict != 1 else ''}",
                     "#1e1608", "#b89030")
            )
        if n_unp_raw:
            self._stats_lay.addWidget(
                chip(f"{n_unp_raw} unpaired RAW", "#0e1828", "#4878a8")
            )
        if n_unp_jpg:
            self._stats_lay.addWidget(
                chip(f"{n_unp_jpg} unpaired JPG", "#0e1e14", "#388850")
            )
        if n_pruned:
            self._stats_lay.addWidget(
                chip(f"✕ {n_pruned} pruned", "#1a0a0a", "#c04040")
            )
        self._stats_lay.addStretch()

    def _refresh_preview(self, groups) -> None:
        while self._preview_lay.count():
            item = self._preview_lay.takeAt(0)
            if item.widget():
                item.widget().deleteLater()

        if not groups:
            msg = (
                "Set a library folder above to see the preview."
                if not self._library_folder
                else "No files to export."
            )
            lbl = QLabel(msg)
            lbl.setStyleSheet(f"font-size:11px; color:{_TEXT_DIM}; font-style:italic;")
            self._preview_lay.addWidget(lbl)
            self._preview_lay.addStretch()
            return

        # Column header
        hdr = QWidget()
        hdr.setStyleSheet("background:transparent;")
        hdr_lay = QHBoxLayout(hdr)
        hdr_lay.setContentsMargins(0, 0, 0, 4)
        hdr_lay.setSpacing(0)
        for text, align, stretch in [
            ("DESTINATION", Qt.AlignLeft,  1),
            ("FILES",       Qt.AlignRight, 0),
        ]:
            l = QLabel(text)
            l.setStyleSheet(
                f"font-size:10px; font-weight:700; color:{_TEXT_DIM};"
                " letter-spacing:0.8px;"
            )
            l.setAlignment(align | Qt.AlignVCenter)
            hdr_lay.addWidget(l, stretch)
        hdr_lay.addSpacing(48)
        self._preview_lay.addWidget(hdr)
        self._preview_lay.addWidget(_h_rule())
        self._preview_lay.addSpacing(2)

        sep_html = f'<span style="color:{_TEXT_DIM}"> / </span>'
        show_pruned = self._chk_pruned.isChecked()

        for g in groups:
            has_pruned = show_pruned and g.pruned_count > 0
            row_w = QWidget()
            row_w.setStyleSheet(
                "background: rgba(180,40,40,0.07); border-radius:3px;"
                if has_pruned else "background:transparent;"
            )
            row_w.setFixedHeight(26)
            row_lay = QHBoxLayout(row_w)
            row_lay.setContentsMargins(4 if has_pruned else 0, 0, 4 if has_pruned else 0, 0)
            row_lay.setSpacing(0)

            type_color = "#5888c0" if g.type_label == "RAW" else "#488858"
            path_lbl = QLabel(
                f'<span style="color:{_TEXT_PRI}">{g.year}</span>{sep_html}'
                f'<span style="color:{_TEXT_SEC}">{g.month_key}</span>{sep_html}'
                f'<span style="color:{type_color}; font-weight:600">{g.type_label}</span>'
            )
            path_lbl.setTextFormat(Qt.RichText)
            path_lbl.setStyleSheet("font-size:12px; background:transparent;")
            path_lbl.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Fixed)

            count_lbl = QLabel(str(g.file_count))
            count_lbl.setStyleSheet(
                f"font-size:12px; color:{_TEXT_SEC}; min-width:28px; background:transparent;"
            )
            count_lbl.setAlignment(Qt.AlignRight | Qt.AlignVCenter)

            row_lay.addWidget(path_lbl, 1)
            row_lay.addWidget(count_lbl)

            if g.conflict_count:
                warn = QLabel(f"  ⚠ {g.conflict_count}")
                warn.setStyleSheet("font-size:11px; color:#b89030; min-width:44px; background:transparent;")
                row_lay.addWidget(warn)
            else:
                row_lay.addSpacing(44)

            if has_pruned:
                prune_lbl = QLabel(f"  ✕ {g.pruned_count}")
                prune_lbl.setStyleSheet(
                    "font-size:11px; color:#c04040; font-weight:600;"
                    " min-width:40px; background:transparent;"
                )
                row_lay.addWidget(prune_lbl)
            else:
                row_lay.addSpacing(40)

            self._preview_lay.addWidget(row_w)

        self._preview_lay.addStretch()

    def _refresh_conflict_section(self) -> None:
        if self._plan is None or self._plan.conflict_count == 0:
            self._conflict_section.hide()
            return

        n = self._plan.conflict_count
        self._conflict_hdr_lbl.setText(
            f"⚠  {n} conflict{'s' if n != 1 else ''} — "
            "file already exists at destination"
        )
        # Reset all bulk buttons to inactive
        for b in self._bulk_btns.values():
            b.setStyleSheet(b.property("_inactive_qss"))
        self._conflict_section.show()

    def _bulk_resolve(self, r: Resolution) -> None:
        if not self._plan:
            return
        for op in self._plan.conflicts:
            op.set_resolution(r)
        # Highlight chosen button
        for res, b in self._bulk_btns.items():
            b.setStyleSheet(
                b.property("_active_qss") if res == r
                else b.property("_inactive_qss")
            )

    # ──────────────────────────────────────────────────────────────────────
    # UI state
    # ──────────────────────────────────────────────────────────────────────

    def _update_export_button(self) -> None:
        can = self._plan is not None and self._plan.total > 0
        self._btn_export.setEnabled(can)
        if can:
            mode  = self._current_mode().capitalize()
            n     = self._plan.total
            label = "Move" if mode == "Move" else "Export"
            self._btn_export.setText(f"{label} {n} File{'s' if n != 1 else ''}")
        else:
            self._btn_export.setText("Export")

        if self._plan:
            records  = self._active_records()
            n_pruned = sum(1 for r in records if r.is_pruned) if self._chk_pruned.isChecked() else 0
            self._refresh_stats(
                self._plan.total, self._plan.conflict_count,
                sum(1 for r in records if r.file_type == FileType.RAW and not r.is_paired),
                sum(1 for r in records if r.file_type == FileType.JPG and not r.is_paired),
                n_pruned,
            )

    # ──────────────────────────────────────────────────────────────────────
    # Library folder picker
    # ──────────────────────────────────────────────────────────────────────

    def _pick_library(self) -> None:
        folder = QFileDialog.getExistingDirectory(
            self, "Set Library Folder",
            str(self._library_folder or QDir.homePath()),
            QFileDialog.ShowDirsOnly | QFileDialog.DontResolveSymlinks,
        )
        if not folder:
            return
        p = Path(folder)
        self._library_folder = p
        self._library_lbl.setText(_elide(p))
        self._library_lbl.setStyleSheet(f"font-size:12px; color:{_TEXT_SEC};")
        self.library_folder_changed.emit(p)
        self._rebuild_plan()

    # ──────────────────────────────────────────────────────────────────────
    # Execution
    # ──────────────────────────────────────────────────────────────────────

    def _start_export(self) -> None:
        if not self._plan or not self._library_folder:
            return

        for w in (self._btn_export, self._btn_cancel, self._chk_pruned,
                  self._btn_copy, self._btn_move):
            w.setEnabled(False)
        self._conflict_section.hide()
        self._progress_widget.show()
        self._progress_bar.setValue(0)
        self._progress_lbl.setText("Starting…")

        self._exec_signals = _ExecSignals()
        self._exec_signals.progress.connect(self._on_exec_progress)
        self._exec_signals.finished.connect(self._on_exec_finished)
        QThreadPool.globalInstance().start(
            _ExecWorker(self._plan, self._current_mode(), self._exec_signals)
        )

    def _on_exec_progress(self, done: int, total: int) -> None:
        if total > 0:
            self._progress_bar.setValue(int(100 * done / total))
            self._progress_lbl.setText(f"{done} / {total} files…")

    def _on_exec_finished(self, succeeded: list, failed: list) -> None:
        self._progress_widget.hide()
        self.exported.emit(succeeded, failed)
        self._show_summary(succeeded, failed)

    def _show_summary(self, succeeded: list, failed: list) -> None:
        while self._preview_lay.count():
            item = self._preview_lay.takeAt(0)
            if item.widget():
                item.widget().deleteLater()

        n_ok, n_fail = len(succeeded), len(failed)
        ok_lbl = QLabel(
            f"✓  {n_ok} file{'s' if n_ok != 1 else ''} exported"
            + (f"  —  {n_fail} failed" if n_fail else "")
        )
        ok_lbl.setStyleSheet(
            f"font-size:14px; font-weight:700;"
            f" color:{'#4a9a60' if not n_fail else '#b89030'};"
        )
        self._preview_lay.addWidget(ok_lbl)

        if n_fail:
            for op, err in failed[:10]:
                e = QLabel(f"  ✗  {op.record.filename}  — {err}")
                e.setStyleSheet("font-size:11px; color:#a04040;")
                self._preview_lay.addWidget(e)
            if n_fail > 10:
                self._preview_lay.addWidget(QLabel(f"  … and {n_fail - 10} more"))

        self._preview_lay.addStretch()

        if self._library_folder:
            self._btn_export.setText("Open Library Folder")
            self._btn_export.setEnabled(True)
            self._btn_export.clicked.disconnect()
            self._btn_export.clicked.connect(self._open_library_folder)

        self._btn_cancel.setText("Close")
        self._btn_cancel.setEnabled(True)

    def _open_library_folder(self) -> None:
        if self._library_folder:
            import subprocess
            try:
                subprocess.Popen(["xdg-open", str(self._library_folder)])
            except Exception:
                pass
