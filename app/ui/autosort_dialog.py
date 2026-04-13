"""
AutoSortDialog — scan any folder and reorganise it into the standard library structure.

Wizard states
-------------
  CONFIGURE  Source + library folders set; Preview button enabled once both are chosen.
  SCANNING   ScanWorker running; spinner/progress in the preview area.
  PREVIEW    LibraryPlan built; tree + optional conflict section visible.
  RUNNING    LibraryPlan.execute() in progress.
  DONE       Summary with "Open Library Folder" option.

Signals
-------
sorted(succeeded, failed)
    Emitted after execution completes.
library_folder_changed(Path)
    Emitted when the user picks a new library folder inside the dialog.
"""
from __future__ import annotations

from pathlib import Path
from typing import List, Optional

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

from app.models.collection import PhotoCollection
from app.models.photo_record import FileType, PhotoRecord
from app.ops.library import LibraryPlan, Resolution
from app.scanning.scanner import ScanWorker


# ── shared style constants (same palette as ExportDialog) ─────────────────── #

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

_STATES = ("configure", "scanning", "preview", "running", "done")


def _h_rule() -> QFrame:
    f = QFrame()
    f.setFrameShape(QFrame.HLine)
    f.setFixedHeight(1)
    f.setStyleSheet(f"background:{_BORDER}; border:none;")
    return f


def _elide(path: Path, max_len: int = 58) -> str:
    s = str(path)
    return s if len(s) <= max_len else "…" + s[-(max_len - 1):]


# ── execute worker (identical pattern to ExportDialog) ────────────────────── #

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


# ── dialog ────────────────────────────────────────────────────────────────── #

class AutoSortDialog(QDialog):
    sorted:                 Signal = Signal(object, object)  # succeeded, failed
    library_folder_changed: Signal = Signal(object)          # Path

    def __init__(
        self,
        working_folder: Optional[Path],
        library_folder: Optional[Path],
        parent=None,
    ) -> None:
        super().__init__(parent)
        self.setWindowTitle("Auto-sort to Library")
        self.setMinimumSize(700, 660)
        self.setModal(True)
        self.setStyleSheet(
            f"QDialog {{ background:{_BG_DIALOG}; color:{_TEXT_PRI}; }}"
            f"QLabel   {{ background:transparent; color:{_TEXT_PRI}; }}"
            f"QScrollArea {{ background:{_BG_SECTION}; border:none; }}"
            f"QScrollBar:vertical {{ background:#0a0a14; width:6px; border:none; }}"
            f"QScrollBar::handle:vertical {{ background:#2a2a44; border-radius:3px; }}"
            f"QScrollBar::add-line:vertical,"
            f"QScrollBar::sub-line:vertical {{ height:0; }}"
        )

        self._source_folder  = working_folder
        self._library_folder = library_folder
        self._scanner:  Optional[ScanWorker]  = None
        self._plan:     Optional[LibraryPlan] = None
        self._collection = PhotoCollection()
        self._state = "configure"

        self._build_ui()
        self._enter_configure()

    # ──────────────────────────────────────────────────────────────────────
    # Construction
    # ──────────────────────────────────────────────────────────────────────

    def _build_ui(self) -> None:
        root = QVBoxLayout(self)
        root.setSpacing(12)
        root.setContentsMargins(16, 16, 16, 16)

        # ── Card: paths ───────────────────────────────────────────────── #
        paths_card = QWidget()
        paths_card.setObjectName("card")
        paths_card.setStyleSheet(f"#card {{ {_CARD_QSS} }}")
        paths_lay = QVBoxLayout(paths_card)
        paths_lay.setContentsMargins(14, 12, 14, 12)
        paths_lay.setSpacing(8)

        self._source_lbl, src_row, self._btn_change_src = self._make_path_row(
            "📂", "SOURCE", self._source_folder, link="change"
        )
        self._library_lbl, lib_row, self._btn_change_lib = self._make_path_row(
            "📁", "LIBRARY", self._library_folder, link="change"
        )

        paths_lay.addLayout(src_row)
        paths_lay.addWidget(_h_rule())
        paths_lay.addLayout(lib_row)
        root.addWidget(paths_card)

        # ── Card: operation ───────────────────────────────────────────── #
        op_card = QWidget()
        op_card.setObjectName("card")
        op_card.setStyleSheet(f"#card {{ {_CARD_QSS} }}")
        op_lay = QVBoxLayout(op_card)
        op_lay.setContentsMargins(14, 14, 14, 14)
        op_lay.setSpacing(12)

        op_title = QLabel("Operation")
        op_title.setStyleSheet(
            f"font-size:10px; font-weight:700; color:{_TEXT_SEC};"
            " letter-spacing:1px;"
        )
        op_lay.addWidget(op_title)

        toggle_row = QHBoxLayout()
        toggle_row.setSpacing(8)
        self._btn_copy = self._make_mode_btn("Copy", "Keep originals in source folder")
        self._btn_move = self._make_mode_btn("Move", "Remove originals after sorting")
        self._btn_copy.setStyleSheet(self._mode_btn_qss(selected=True))
        self._btn_move.setStyleSheet(self._mode_btn_qss(selected=False))
        self._btn_copy.clicked.connect(lambda: self._select_mode("copy"))
        self._btn_move.clicked.connect(lambda: self._select_mode("move"))
        toggle_row.addWidget(self._btn_copy)
        toggle_row.addWidget(self._btn_move)
        op_lay.addLayout(toggle_row)
        root.addWidget(op_card)

        # ── Preview / scan area ───────────────────────────────────────── #
        self._preview_scroll = QScrollArea()
        self._preview_scroll.setWidgetResizable(True)
        self._preview_scroll.setStyleSheet(
            f"QScrollArea {{ background:{_BG_SECTION};"
            f" border:1px solid {_BORDER}; border-radius:6px; }}"
            f"QScrollBar:vertical {{ background:#0a0a14; width:6px; border:none; }}"
            f"QScrollBar::handle:vertical {{ background:#2a2a44; border-radius:3px; }}"
            f"QScrollBar::add-line:vertical,"
            f"QScrollBar::sub-line:vertical {{ height:0; }}"
        )
        self._preview_inner = QWidget()
        self._preview_inner.setStyleSheet(f"background:{_BG_SECTION};")
        self._preview_lay = QVBoxLayout(self._preview_inner)
        self._preview_lay.setContentsMargins(14, 10, 14, 10)
        self._preview_lay.setSpacing(3)
        self._preview_scroll.setWidget(self._preview_inner)
        root.addWidget(self._preview_scroll, 1)

        # ── Stats row (between preview and conflict) ──────────────────── #
        self._stats_widget = QWidget()
        self._stats_widget.setStyleSheet("background:transparent;")
        self._stats_lay = QHBoxLayout(self._stats_widget)
        self._stats_lay.setContentsMargins(2, 0, 2, 0)
        self._stats_lay.setSpacing(8)
        self._stats_widget.hide()
        root.addWidget(self._stats_widget)

        # ── Conflict bar (compact — no per-file list) ─────────────────── #
        self._conflict_section = QWidget()
        self._conflict_section.setObjectName("cbar")
        self._conflict_section.setStyleSheet(
            "#cbar { background:#18100c; border:1px solid rgba(200,140,0,0.28);"
            " border-radius:6px; }"
        )
        cbar_lay = QHBoxLayout(self._conflict_section)
        cbar_lay.setContentsMargins(14, 10, 14, 10)
        cbar_lay.setSpacing(12)

        self._conflict_hdr_lbl = QLabel()
        self._conflict_hdr_lbl.setStyleSheet(
            "font-size:12px; color:#c8a040; font-weight:600;"
        )
        cbar_lay.addWidget(self._conflict_hdr_lbl, 1)

        bulk_lbl = QLabel("Apply to all:")
        bulk_lbl.setStyleSheet(f"font-size:11px; color:{_TEXT_SEC};")
        cbar_lay.addWidget(bulk_lbl)

        self._bulk_btns: dict[Resolution, QPushButton] = {}
        for label, res in (("Skip", Resolution.SKIP),
                           ("Overwrite", Resolution.OVERWRITE),
                           ("Rename", Resolution.RENAME)):
            b = QPushButton(label)
            b.setFixedHeight(26)
            b.setStyleSheet(
                "QPushButton { background:#1e1a10; color:#a09060;"
                " border:1px solid rgba(200,140,0,0.25); border-radius:3px;"
                " font-size:11px; padding:0 12px; }"
                "QPushButton:hover { background:#2a2010; color:#d0b060;"
                " border-color:rgba(200,140,0,0.60); }"
                "QPushButton[active=true] { background:rgba(200,140,0,0.20);"
                " color:#e0c070; border-color:rgba(200,140,0,0.70); }"
            )
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
        self._progress_lbl = QLabel("")
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

        self._btn_primary = QPushButton("Preview")
        self._btn_primary.setFixedHeight(36)
        self._btn_primary.setMinimumWidth(170)
        self._btn_primary.setCursor(Qt.PointingHandCursor)
        self._btn_primary.setStyleSheet(self._primary_btn_qss())
        self._btn_primary.clicked.connect(self._on_primary)

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

        footer.addWidget(self._btn_primary)
        footer.addStretch()
        footer.addWidget(self._btn_cancel)
        root.addLayout(footer)

        # ── Wire folder pickers ───────────────────────────────────────── #
        self._btn_change_src.clicked.connect(self._pick_source)
        self._btn_change_lib.clicked.connect(self._pick_library)

    # ──────────────────────────────────────────────────────────────────────
    # Widget helpers
    # ──────────────────────────────────────────────────────────────────────

    def _make_path_row(self, emoji: str, label: str, path: Optional[Path],
                       link: str = ""):
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

        path_text = _elide(path) if path else "click to set…"
        path_lbl = QLabel(path_text)
        path_lbl.setStyleSheet(
            f"font-size:12px; color:{ _TEXT_SEC if path else '#2e2e4a'};"
            + (" font-style:italic;" if not path else "")
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
        btn = QPushButton(f"{title}\n{subtitle}")
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

    @staticmethod
    def _primary_btn_qss() -> str:
        return (
            "QPushButton { background:#1e4d96; color:#a8d0ff;"
            " border:1px solid rgba(100,170,255,0.40);"
            " border-radius:5px; font-size:13px; font-weight:700; padding:0 24px; }"
            "QPushButton:hover { background:#2560b8; color:#cce4ff;"
            " border-color:rgba(100,170,255,0.70); }"
            "QPushButton:disabled { background:#111128; color:#303050;"
            " border-color:rgba(100,170,255,0.06); }"
        )

    def _select_mode(self, mode: str) -> None:
        self._btn_copy.setStyleSheet(self._mode_btn_qss(mode == "copy"))
        self._btn_move.setStyleSheet(self._mode_btn_qss(mode == "move"))

    def _current_mode(self) -> str:
        return "move" if "rgba(255,109,0,0.12)" in self._btn_move.styleSheet() else "copy"

    # ──────────────────────────────────────────────────────────────────────
    # State machine
    # ──────────────────────────────────────────────────────────────────────

    def _enter_configure(self) -> None:
        self._state = "configure"
        self._stats_widget.hide()
        self._conflict_section.hide()
        self._progress_widget.hide()
        self._btn_primary.setText("Preview")
        self._btn_primary.setEnabled(
            bool(self._source_folder and self._library_folder)
        )
        self._btn_copy.setEnabled(True)
        self._btn_move.setEnabled(True)
        self._btn_cancel.setText("Cancel")
        self._btn_cancel.setEnabled(True)
        self._clear_preview()
        self._show_preview_placeholder()

    def _enter_scanning(self) -> None:
        self._state = "scanning"
        self._btn_primary.setEnabled(False)
        self._btn_copy.setEnabled(False)
        self._btn_move.setEnabled(False)
        self._btn_change_src.setEnabled(False)
        self._btn_change_lib.setEnabled(False)
        self._conflict_section.hide()
        self._stats_widget.hide()
        self._clear_preview()
        self._show_scan_placeholder()

    def _enter_preview(self) -> None:
        self._state = "preview"
        self._btn_change_src.setEnabled(True)
        self._btn_change_lib.setEnabled(True)
        self._btn_copy.setEnabled(True)
        self._btn_move.setEnabled(True)
        self._progress_widget.hide()

        if self._plan and self._plan.total > 0:
            n = self._plan.total
            mode = "Move" if self._current_mode() == "move" else "Sort"
            self._btn_primary.setText(f"{mode} {n} File{'s' if n != 1 else ''}")
            self._btn_primary.setEnabled(True)
        else:
            self._btn_primary.setText("Nothing to sort")
            self._btn_primary.setEnabled(False)

        self._refresh_stats()
        self._refresh_preview()
        self._refresh_conflict_section()
        self._stats_widget.show()

    def _enter_running(self) -> None:
        self._state = "running"
        for w in (self._btn_primary, self._btn_cancel,
                  self._btn_copy, self._btn_move,
                  self._btn_change_src, self._btn_change_lib):
            w.setEnabled(False)
        self._conflict_section.hide()
        self._progress_widget.show()
        self._progress_bar.setValue(0)
        self._progress_lbl.setText("Starting…")

    def _enter_done(self, succeeded: list, failed: list) -> None:
        self._state = "done"
        self._progress_widget.hide()
        self._stats_widget.hide()
        self.sorted.emit(succeeded, failed)
        self._show_summary(succeeded, failed)

        if self._library_folder:
            self._btn_primary.setText("Open Library Folder")
            self._btn_primary.setEnabled(True)
            self._btn_primary.clicked.disconnect()
            self._btn_primary.clicked.connect(self._open_library_folder)

        self._btn_cancel.setText("Close")
        self._btn_cancel.setEnabled(True)

    # ──────────────────────────────────────────────────────────────────────
    # Primary button dispatch
    # ──────────────────────────────────────────────────────────────────────

    def _on_primary(self) -> None:
        if self._state == "configure":
            self._start_scan()
        elif self._state == "preview":
            self._start_sort()

    # ──────────────────────────────────────────────────────────────────────
    # Folder pickers
    # ──────────────────────────────────────────────────────────────────────

    def _pick_source(self) -> None:
        folder = QFileDialog.getExistingDirectory(
            self, "Choose Source Folder",
            str(self._source_folder or QDir.homePath()),
            QFileDialog.ShowDirsOnly | QFileDialog.DontResolveSymlinks,
        )
        if not folder:
            return
        p = Path(folder)
        self._source_folder = p
        self._source_lbl.setText(_elide(p))
        self._source_lbl.setStyleSheet(f"font-size:12px; color:{_TEXT_SEC};")
        # Reset to configure if we had already previewed
        if self._state in ("preview",):
            self._enter_configure()
        else:
            self._btn_primary.setEnabled(
                bool(self._source_folder and self._library_folder)
            )

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
        if self._state in ("preview",):
            self._enter_configure()
        else:
            self._btn_primary.setEnabled(
                bool(self._source_folder and self._library_folder)
            )

    # ──────────────────────────────────────────────────────────────────────
    # Scanning
    # ──────────────────────────────────────────────────────────────────────

    def _start_scan(self) -> None:
        if not self._source_folder or not self._library_folder:
            return

        self._collection = PhotoCollection()
        self._enter_scanning()

        self._scanner = ScanWorker(self._source_folder, recursive=True)
        self._scanner.file_found.connect(self._on_file_found)
        self._scanner.progress.connect(self._on_scan_progress)
        self._scanner.scan_complete.connect(self._on_scan_complete)
        self._scanner.scan_error.connect(self._on_scan_error)
        self._scanner.start()

    def _on_file_found(self, record: PhotoRecord) -> None:
        self._collection.add(record)

    def _on_scan_progress(self, count: int) -> None:
        self._update_scan_placeholder(count)

    def _on_scan_complete(self, count: int) -> None:
        self._collection.build_pairs(auto_detect=True)
        records = self._collection.all()
        self._plan = LibraryPlan(
            records,
            self._library_folder,
            self._collection.find_pair,
        )
        self._enter_preview()

    def _on_scan_error(self, msg: str) -> None:
        self._clear_preview()
        err = QLabel(f"Scan error: {msg}")
        err.setStyleSheet("font-size:11px; color:#a04040;")
        self._preview_lay.addWidget(err)
        self._preview_lay.addStretch()
        self._btn_primary.setEnabled(True)
        self._btn_primary.setText("Retry Preview")
        self._btn_change_src.setEnabled(True)
        self._btn_change_lib.setEnabled(True)
        self._state = "configure"

    # ──────────────────────────────────────────────────────────────────────
    # Preview area helpers
    # ──────────────────────────────────────────────────────────────────────

    def _clear_preview(self) -> None:
        while self._preview_lay.count():
            item = self._preview_lay.takeAt(0)
            if item.widget():
                item.widget().deleteLater()

    def _show_preview_placeholder(self) -> None:
        if not self._source_folder or not self._library_folder:
            msg = "Set source and library folders above, then click Preview."
        else:
            msg = "Click Preview to scan the source folder."
        lbl = QLabel(msg)
        lbl.setStyleSheet(f"font-size:11px; color:{_TEXT_DIM}; font-style:italic;")
        self._preview_lay.addWidget(lbl)
        self._preview_lay.addStretch()

    def _show_scan_placeholder(self) -> None:
        self._scan_status_lbl = QLabel("Scanning…  0 files found")
        self._scan_status_lbl.setStyleSheet(f"font-size:12px; color:{_TEXT_SEC};")
        self._preview_lay.addWidget(self._scan_status_lbl)
        self._preview_lay.addStretch()

    def _update_scan_placeholder(self, count: int) -> None:
        if hasattr(self, "_scan_status_lbl"):
            self._scan_status_lbl.setText(f"Scanning…  {count} files found")

    def _refresh_stats(self) -> None:
        while self._stats_lay.count():
            item = self._stats_lay.takeAt(0)
            if item.widget():
                item.widget().deleteLater()

        if not self._plan:
            return

        records = self._collection.all()
        n_unp_raw = sum(1 for r in records if r.file_type == FileType.RAW and not r.is_paired)
        n_unp_jpg = sum(1 for r in records if r.file_type == FileType.JPG and not r.is_paired)

        def chip(text: str, bg: str, fg: str) -> QLabel:
            l = QLabel(text)
            l.setStyleSheet(
                f"background:{bg}; color:{fg}; border-radius:4px;"
                " padding:3px 10px; font-size:11px; font-weight:600;"
            )
            return l

        mode = self._current_mode()
        n = self._plan.total
        self._stats_lay.addWidget(
            chip(f"{n} file{'s' if n != 1 else ''} to {mode}", "#181830", "#9898c0")
        )
        if self._plan.conflict_count:
            self._stats_lay.addWidget(
                chip(f"⚠  {self._plan.conflict_count} conflict{'s' if self._plan.conflict_count != 1 else ''}",
                     "#1e1608", "#b89030")
            )
        if n_unp_raw:
            self._stats_lay.addWidget(chip(f"{n_unp_raw} unpaired RAW", "#0e1828", "#4878a8"))
        if n_unp_jpg:
            self._stats_lay.addWidget(chip(f"{n_unp_jpg} unpaired JPG", "#0e1e14", "#388850"))
        self._stats_lay.addStretch()

    def _refresh_preview(self) -> None:
        self._clear_preview()

        if not self._plan:
            self._show_preview_placeholder()
            return

        groups = self._plan.grouped()
        if not groups:
            lbl = QLabel("No files to sort.")
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
        for text, align, stretch in [("DESTINATION", Qt.AlignLeft, 1),
                                      ("FILES", Qt.AlignRight, 0)]:
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
        for g in groups:
            row_w = QWidget()
            row_w.setStyleSheet("background:transparent;")
            row_w.setFixedHeight(26)
            row_lay = QHBoxLayout(row_w)
            row_lay.setContentsMargins(0, 0, 0, 0)
            row_lay.setSpacing(0)

            tc = "#5888c0" if g.type_label == "RAW" else "#488858"
            path_lbl = QLabel(
                f'<span style="color:{_TEXT_PRI}">{g.year}</span>{sep_html}'
                f'<span style="color:{_TEXT_SEC}">{g.month_key}</span>{sep_html}'
                f'<span style="color:{tc}; font-weight:600">{g.type_label}</span>'
            )
            path_lbl.setTextFormat(Qt.RichText)
            path_lbl.setStyleSheet("font-size:12px;")
            path_lbl.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Fixed)

            count_lbl = QLabel(str(g.file_count))
            count_lbl.setStyleSheet(f"font-size:12px; color:{_TEXT_SEC}; min-width:28px;")
            count_lbl.setAlignment(Qt.AlignRight | Qt.AlignVCenter)

            row_lay.addWidget(path_lbl, 1)
            row_lay.addWidget(count_lbl)

            if g.conflict_count:
                warn = QLabel(f"  ⚠ {g.conflict_count}")
                warn.setStyleSheet("font-size:11px; color:#b89030; min-width:44px;")
                row_lay.addWidget(warn)
            else:
                row_lay.addSpacing(44)

            self._preview_lay.addWidget(row_w)

        self._preview_lay.addStretch()

    def _refresh_conflict_section(self) -> None:
        if not self._plan or self._plan.conflict_count == 0:
            self._conflict_section.hide()
            return

        n = self._plan.conflict_count
        self._conflict_hdr_lbl.setText(
            f"⚠  {n} file{'s' if n != 1 else ''} already exist at destination"
        )
        # Reset all bulk buttons to unselected
        for b in self._bulk_btns.values():
            b.setProperty("active", False)
            b.style().unpolish(b)
            b.style().polish(b)
        self._conflict_section.show()

    def _bulk_resolve(self, r: Resolution) -> None:
        if not self._plan:
            return
        for op in self._plan.conflicts:
            op.set_resolution(r)
        # Highlight the chosen button
        for res, b in self._bulk_btns.items():
            b.setProperty("active", res == r)
            b.style().unpolish(b)
            b.style().polish(b)

    # ──────────────────────────────────────────────────────────────────────
    # Execution
    # ──────────────────────────────────────────────────────────────────────

    def _start_sort(self) -> None:
        if not self._plan:
            return
        self._enter_running()
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
        self._enter_done(succeeded, failed)

    def _show_summary(self, succeeded: list, failed: list) -> None:
        self._clear_preview()
        n_ok, n_fail = len(succeeded), len(failed)
        ok_lbl = QLabel(
            f"✓  {n_ok} file{'s' if n_ok != 1 else ''} sorted"
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

    def _open_library_folder(self) -> None:
        if self._library_folder:
            import subprocess
            try:
                subprocess.Popen(["xdg-open", str(self._library_folder)])
            except Exception:
                pass

    # ──────────────────────────────────────────────────────────────────────
    # Cleanup
    # ──────────────────────────────────────────────────────────────────────

    def closeEvent(self, event) -> None:
        if self._scanner and self._scanner.isRunning():
            self._scanner.cancel()
        super().closeEvent(event)

    def reject(self) -> None:
        if self._scanner and self._scanner.isRunning():
            self._scanner.cancel()
        super().reject()
