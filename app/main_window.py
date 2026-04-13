"""
MainWindow — top-level application window.

Phase 2 additions on top of Phase 1:
  • ThumbnailGenerator (background pixmap generation + in-memory cache)
  • FilterBar (RAW / JPG / Paired / Unpaired toggles)
  • ThumbnailGridView (icon-mode grid with custom delegate)
  • QStackedWidget to switch between List and Grid views
  • Toolbar view-toggle buttons (List | Grid)
  • Both views share the same filter state; filter bar drives both proxies
  • Status bar shows "Showing X of Y" counts
"""
from __future__ import annotations

from pathlib import Path

from typing import List

from PySide6.QtCore import QDir, QSettings, QSize, QTimer, Qt
from PySide6.QtGui import QAction, QActionGroup
from PySide6.QtWidgets import (
    QFileDialog,
    QHBoxLayout,
    QLabel,
    QMainWindow,
    QPushButton,
    QSizePolicy,
    QSlider,
    QStackedWidget,
    QStatusBar,
    QToolBar,
    QVBoxLayout,
    QWidget,
)

from app.models.collection import PhotoCollection
from app.models.photo_record import FileType
from app.scanning.scanner import ScanWorker
from app.thumbnails.generator import ThumbnailGenerator
from app.ui.file_list import FileListWidget
from app.ui.filter_bar import FilterBar
from app.ui.icons import icon, pixmap as icon_pixmap
from app.ui.import_dialog import ImportDialog
from app.ui.prune_review import PruneReviewDialog
from app.ui.pair_dialog import PairDialog
from app.ui.separate_dialog import SeparateDialog
from app.ui.shortcuts_dialog import KeyboardShortcutsDialog
from app.ui.grouped_grid import GroupedGridView
from app.ui.viewer import ImageViewer

_SETTINGS_ORG = "PhotoPipeline"
_SETTINGS_APP = "PhotoPipeline"
_PRUNE_FILE   = ".photo_pipeline.json"       # sidecar: prune marks
_PAIRS_FILE   = ".photo-pipeline-pairs.json" # sidecar: persistent pair marks


def _set_text_beside_icon(toolbar, action) -> None:
    """Set a single toolbar button to show text beside its icon."""
    from PySide6.QtWidgets import QToolButton
    btn = toolbar.widgetForAction(action)
    if isinstance(btn, QToolButton):
        btn.setToolButtonStyle(Qt.ToolButtonTextBesideIcon)

_THUMB_SIZE = 160
_VIEW_LIST = 0
_VIEW_GRID = 1


class MainWindow(QMainWindow):
    def __init__(self) -> None:
        super().__init__()
        self.setWindowTitle("Photo Pipeline")
        from app.ui.icons import tinted_icon
        self.setWindowIcon(tinted_icon("retro-camera", "#ff6d00", size=48))
        self.resize(1200, 780)

        self._collection = PhotoCollection()
        self._scanner: ScanWorker | None = None
        self._generator = ThumbnailGenerator(thumb_size=_THUMB_SIZE, parent=self)
        self._viewer: ImageViewer | None = None
        self._import_mode: bool = False
        self._recursive: bool = True
        self._current_folder: Path | None = None

        # Scan buffer: accumulate file_found records and flush in batches
        # to avoid thousands of individual model-insert signals per second.
        self._scan_buffer: List = []
        self._flush_timer = QTimer(self)
        self._flush_timer.setInterval(150)   # flush every 150 ms while scanning
        self._flush_timer.timeout.connect(self._flush_scan_buffer)

        self._shortcuts_dlg: KeyboardShortcutsDialog | None = None

        self._build_menu()
        self._build_toolbar()
        self._build_central()
        self._build_statusbar()
        # Wire Ctrl+scroll zoom now that both toolbar and grid exist
        self._grid_view.thumb_size_changed.connect(self._zoom_slider.setValue)
        self._restore_session()

    # ------------------------------------------------------------------ #
    # Construction                                                         #
    # ------------------------------------------------------------------ #

    def _build_menu(self) -> None:
        mb = self.menuBar()

        file_menu = mb.addMenu("&File")

        import_act = QAction("&Import…", self)
        import_act.setShortcut("Ctrl+I")
        import_act.setStatusTip(
            "Choose a folder to import — files will be auto-separated into RAW/ and JPG/"
        )
        import_act.triggered.connect(self.import_folder)
        file_menu.addAction(import_act)

        open_act = QAction("&Open…", self)
        open_act.setShortcut("Ctrl+O")
        open_act.setStatusTip("Open an already-organised working directory (no separation)")
        open_act.triggered.connect(self.open_folder)
        file_menu.addAction(open_act)

        file_menu.addSeparator()

        quit_act = QAction("&Quit", self)
        quit_act.setShortcut("Ctrl+Q")
        quit_act.triggered.connect(self.close)
        file_menu.addAction(quit_act)

        prune_menu = mb.addMenu("&Prune")

        review_act = QAction("&Review Pruned…", self)
        review_act.setShortcut("Ctrl+R")
        review_act.setStatusTip("Review pruned files and commit to Trash")
        review_act.triggered.connect(self._open_review)
        prune_menu.addAction(review_act)

        separate_act = QAction("&Sort into RAW/JPG…", self)
        separate_act.setShortcut("Ctrl+Shift+S")
        separate_act.setStatusTip("Sort files into RAW/ and JPG/ subfolders")
        separate_act.triggered.connect(self._open_sort)
        prune_menu.addAction(separate_act)

        pair_act = QAction("&Pair RAW + JPG…", self)
        pair_act.setShortcut("Ctrl+Shift+P")
        pair_act.setStatusTip("Pair RAW and JPG files by stem and save to sidecar")
        pair_act.triggered.connect(self._open_pair)
        prune_menu.addAction(pair_act)

        help_menu = mb.addMenu("&Help")
        shortcuts_act = QAction("&Keyboard Shortcuts…", self)
        shortcuts_act.setShortcut("?")
        shortcuts_act.triggered.connect(self._show_shortcuts)
        help_menu.addAction(shortcuts_act)

        view_menu = mb.addMenu("&View")

        list_act = QAction("&List", self)
        list_act.setShortcut("Ctrl+1")
        list_act.triggered.connect(lambda: self._switch_view(_VIEW_LIST))
        view_menu.addAction(list_act)

        grid_act = QAction("&Grid", self)
        grid_act.setShortcut("Ctrl+2")
        grid_act.triggered.connect(lambda: self._switch_view(_VIEW_GRID))
        view_menu.addAction(grid_act)

    def _build_toolbar(self) -> None:
        tb = self._toolbar = QToolBar("Main")
        tb.setObjectName("MainToolBar")
        tb.setMovable(False)
        tb.setToolButtonStyle(Qt.ToolButtonIconOnly)
        tb.setIconSize(QSize(18, 18))
        tb.setStyleSheet("""
            QToolBar {
                background: #0e0e1a;
                border-bottom: 1px solid rgba(255,109,0,0.12);
                padding: 3px 4px;
                spacing: 1px;
            }
            QToolButton {
                background: transparent;
                border: none;
                border-radius: 5px;
                padding: 5px 7px;
                min-width: 28px;
                min-height: 28px;
                color: #c8c8d8;
            }
            QToolButton:hover   { background: rgba(255,109,0,0.10); }
            QToolButton:pressed { background: rgba(255,109,0,0.18); }
            QToolButton:checked {
                background: rgba(255,109,0,0.15);
                border: 1px solid rgba(255,109,0,0.45);
            }
            QToolBar::separator {
                background: rgba(255,109,0,0.12);
                width: 1px;
                margin: 5px 4px;
            }
        """)
        self.addToolBar(tb)
        tb.toggleViewAction().setVisible(False)   # hide from context menu so it can't be accidentally hidden

        import_act_tb = QAction(icon("file-import"), "Import", self)
        import_act_tb.setShortcut("Ctrl+I")
        import_act_tb.setToolTip("Import folder — auto-separate RAW/JPG  (Ctrl+I)")
        import_act_tb.triggered.connect(self.import_folder)
        tb.addAction(import_act_tb)

        open_act_tb = QAction(icon("folder-open"), "Open", self)
        open_act_tb.setShortcut("Ctrl+O")
        open_act_tb.setToolTip("Open working directory  (Ctrl+O)")
        open_act_tb.triggered.connect(self.open_folder)
        tb.addAction(open_act_tb)

        tb.addSeparator()

        review_act_tb = QAction(icon("trash"), "Review", self)
        review_act_tb.setShortcut("Ctrl+R")
        review_act_tb.setToolTip("Review pruned files → Trash  (Ctrl+R)")
        review_act_tb.triggered.connect(self._open_review)
        tb.addAction(review_act_tb)

        sort_act_tb = QAction(icon("fork"), "Sort", self)
        sort_act_tb.setShortcut("Ctrl+Shift+S")
        sort_act_tb.setToolTip("Sort into RAW/ and JPG/ subfolders  (Ctrl+Shift+S)")
        sort_act_tb.triggered.connect(self._open_sort)
        tb.addAction(sort_act_tb)
        _set_text_beside_icon(tb, sort_act_tb)

        pair_act_tb = QAction(icon("link"), "Pair", self)
        pair_act_tb.setShortcut("Ctrl+Shift+P")
        pair_act_tb.setToolTip("Pair RAW+JPG by stem and save  (Ctrl+Shift+P)")
        pair_act_tb.triggered.connect(self._open_pair)
        tb.addAction(pair_act_tb)
        _set_text_beside_icon(tb, pair_act_tb)

        tb.addSeparator()

        self._act_list = QAction(icon("bullet-list"), "List", self)
        self._act_list.setCheckable(True)
        self._act_list.setChecked(True)
        self._act_list.setShortcut("Ctrl+1")
        self._act_list.setToolTip("List view  (Ctrl+1)")
        self._act_list.triggered.connect(lambda: self._switch_view(_VIEW_LIST))
        tb.addAction(self._act_list)
        _set_text_beside_icon(tb, self._act_list)

        self._act_grid = QAction(icon("grid"), "Grid", self)
        self._act_grid.setCheckable(True)
        self._act_grid.setChecked(False)
        self._act_grid.setShortcut("Ctrl+2")
        self._act_grid.setToolTip("Grid view  (Ctrl+2)")
        self._act_grid.triggered.connect(lambda: self._switch_view(_VIEW_GRID))
        tb.addAction(self._act_grid)
        _set_text_beside_icon(tb, self._act_grid)

        grp = QActionGroup(self)
        grp.setExclusive(True)
        grp.addAction(self._act_list)
        grp.addAction(self._act_grid)

        # ── select + cull buttons ─────────────────────────────────────── #
        tb.addSeparator()
        self._act_select = QAction(icon("plus"), "Select", self)
        self._act_select.setCheckable(True)
        self._act_select.setShortcut("S")
        self._act_select.setToolTip("Multi-select mode  (S)")
        self._act_select.toggled.connect(self._on_select_mode_toggled)
        tb.addAction(self._act_select)
        _set_text_beside_icon(tb, self._act_select)

        self._act_cull = QAction(icon("times-circle"), "Prune", self)
        self._act_cull.setToolTip("Mark selected as pruned  (P)")
        self._act_cull.triggered.connect(self._toggle_prune_selected)
        tb.addAction(self._act_cull)
        _set_text_beside_icon(tb, self._act_cull)

        # ── spacer pushes zoom to the right ──────────────────────────── #
        spacer = QWidget()
        spacer.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Preferred)
        spacer.setStyleSheet("background: transparent;")
        self._zoom_spacer_action = tb.addWidget(spacer)

        # ── grid zoom controls (hidden in list view) ──────────────────── #
        self._zoom_widget = self._build_zoom_widget(tb)
        self._zoom_tb_action = tb.addWidget(self._zoom_widget)

    def _build_central(self) -> None:
        root = QWidget()
        self.setCentralWidget(root)
        layout = QVBoxLayout(root)
        layout.setContentsMargins(8, 6, 8, 6)
        layout.setSpacing(4)

        # ── filter bar ────────────────────────────────────────────── #
        self._filter_bar = FilterBar()
        layout.addWidget(self._filter_bar)
        self._filter_bar.filter_changed.connect(self._on_filter_changed)

        # ── stacked views ─────────────────────────────────────────── #
        self._stack = QStackedWidget()
        layout.addWidget(self._stack)

        self._file_list = FileListWidget()
        self._grid_view = GroupedGridView(self._generator, thumb_size=_THUMB_SIZE)

        self._stack.addWidget(self._file_list)   # index 0 → list
        self._stack.addWidget(self._grid_view)   # index 1 → grid

        self._file_list.item_activated.connect(self._on_item_activated)
        self._grid_view.item_activated.connect(self._on_item_activated)

        self._file_list.prune_toggled.connect(self._on_prune_toggled)
        self._grid_view.prune_toggled.connect(self._on_prune_toggled)

        # Keep toolbar Select button in sync when mode exits via Done/Escape
        self._grid_view.selection_mode_changed.connect(self._on_grid_select_mode_changed)

        self._stack.setCurrentIndex(_VIEW_LIST)

    def _build_zoom_widget(self, toolbar: "QToolBar") -> QWidget:
        """Magnifying-glass icon + − [slider] + widget in the toolbar."""
        w = QWidget()
        w.setStyleSheet("QWidget { background: transparent; }")
        w.setSizePolicy(QSizePolicy.Fixed, QSizePolicy.Fixed)
        lay = QHBoxLayout(w)
        lay.setContentsMargins(4, 0, 4, 0)
        lay.setSpacing(3)

        # ── zoom icon ─────────────────────────────────────────────── #
        ic_lbl = QLabel()
        ic_lbl.setPixmap(icon_pixmap("search", size=16))
        ic_lbl.setStyleSheet("background: transparent;")
        ic_lbl.setFixedSize(18, 28)
        ic_lbl.setAlignment(Qt.AlignCenter)

        # ── buttons ───────────────────────────────────────────────── #
        _btn_qss = (
            "QPushButton {"
            "  background: rgba(255,109,0,0.08);"
            "  color: #8888a8;"
            "  border: 1px solid rgba(255,109,0,0.18);"
            "  border-radius: 4px;"
            "  font-size: 14px; font-weight: bold;"
            "  padding: 0;"
            "}"
            "QPushButton:hover {"
            "  background: rgba(255,109,0,0.20); color: #f0f0f0;"
            "  border-color: rgba(255,109,0,0.45);"
            "}"
            "QPushButton:pressed { background: rgba(255,109,0,0.30); }"
        )
        btn_out = QPushButton("−")
        btn_out.setFixedSize(26, 26)
        btn_out.setStyleSheet(_btn_qss)

        btn_in = QPushButton("+")
        btn_in.setFixedSize(26, 26)
        btn_in.setStyleSheet(_btn_qss)

        # ── slider ────────────────────────────────────────────────── #
        self._zoom_slider = QSlider(Qt.Horizontal)
        self._zoom_slider.setRange(80, 280)
        self._zoom_slider.setSingleStep(20)
        self._zoom_slider.setPageStep(20)
        self._zoom_slider.setValue(_THUMB_SIZE)
        self._zoom_slider.setFixedWidth(100)
        self._zoom_slider.setStyleSheet(
            "QSlider::groove:horizontal {"
            "  background: #1e1e2e; border-radius: 3px; height: 4px; }"
            "QSlider::handle:horizontal {"
            "  background: #ff6d00; border-radius: 5px;"
            "  width: 12px; height: 12px; margin: -4px 0; }"
            "QSlider::sub-page:horizontal {"
            "  background: rgba(255,109,0,0.45); border-radius: 3px; }"
        )

        btn_out.clicked.connect(
            lambda: self._zoom_slider.setValue(max(80,  self._zoom_slider.value() - 20)))
        btn_in.clicked.connect(
            lambda: self._zoom_slider.setValue(min(280, self._zoom_slider.value() + 20)))
        self._zoom_slider.valueChanged.connect(self._on_thumb_size_changed)

        lay.addWidget(ic_lbl)
        lay.addSpacing(5)
        lay.addWidget(btn_out)
        lay.addWidget(self._zoom_slider)
        lay.addWidget(btn_in)
        return w

    def _build_statusbar(self) -> None:
        self._statusbar = QStatusBar()
        self._statusbar.setStyleSheet(
            "QStatusBar {"
            "  background: #0e0e1a;"
            "  border-top: 1px solid rgba(255,109,0,0.10);"
            "  color: #7878a0; font-size: 11px;"
            "}"
            "QStatusBar::item { border: none; }"
        )
        self.setStatusBar(self._statusbar)

        # ── stats chips (permanent — right-aligned, never hidden by messages) #
        stats_container = QWidget()
        stats_container.setStyleSheet("QWidget { background: transparent; }")
        sc_lay = QHBoxLayout(stats_container)
        sc_lay.setContentsMargins(0, 0, 6, 0)
        sc_lay.setSpacing(5)

        def _chip(bg: str, text_color: str = "#c8c8d8") -> QLabel:
            lbl = QLabel()
            lbl.setStyleSheet(
                f"QLabel {{ background: {bg}; color: {text_color}; "
                f"border-radius: 3px; padding: 1px 8px; "
                f"font-size: 11px; font-weight: 600; }}"
            )
            return lbl

        self._lbl_total  = _chip("#161620", "#7878a0")
        self._lbl_raw    = _chip("#0d1a30", "#5a8acc")
        self._lbl_jpg    = _chip("#0d2014", "#4a9a5a")
        self._lbl_paired = _chip("rgba(255,109,0,0.12)", "#ff6d00")
        self._lbl_pruned = _chip("#2a0d0d", "#cc4848")

        for lbl in (self._lbl_total, self._lbl_raw, self._lbl_jpg,
                    self._lbl_paired, self._lbl_pruned):
            sc_lay.addWidget(lbl)

        self._statusbar.addPermanentWidget(stats_container)
        self._statusbar.showMessage("Ready — open a folder to begin")

    # ------------------------------------------------------------------ #
    # Thumbnail size                                                       #
    # ------------------------------------------------------------------ #

    # ------------------------------------------------------------------ #
    # View switching                                                       #
    # ------------------------------------------------------------------ #

    def _switch_view(self, idx: int) -> None:
        self._stack.setCurrentIndex(idx)
        self._act_list.setChecked(idx == _VIEW_LIST)
        self._act_grid.setChecked(idx == _VIEW_GRID)
        self._zoom_tb_action.setVisible(idx == _VIEW_GRID)
        self._zoom_spacer_action.setVisible(idx == _VIEW_GRID)
        self._update_status_count()

    def _on_thumb_size_changed(self, size: int) -> None:
        snapped = max(80, min(280, (size // 20) * 20))
        self._generator.thumb_size = snapped
        self._grid_view.set_thumb_size(snapped)

    # ------------------------------------------------------------------ #
    # Filtering                                                            #
    # ------------------------------------------------------------------ #

    def _on_filter_changed(self, state) -> None:
        self._file_list.apply_filter(state)
        self._grid_view.apply_filter(state)
        self._update_status_count()

    # ------------------------------------------------------------------ #
    # Scanning                                                             #
    # ------------------------------------------------------------------ #

    def import_folder(self) -> None:
        dlg = ImportDialog(parent=self)
        if dlg.exec() and dlg.chosen_path():
            self._recursive = dlg.recursive()
            self._import_mode = True
            self._start_scan(dlg.chosen_path())

    def open_folder(self) -> None:
        folder = QFileDialog.getExistingDirectory(
            self,
            "Open Working Directory",
            QDir.homePath(),
            QFileDialog.ShowDirsOnly | QFileDialog.DontResolveSymlinks,
        )
        if folder:
            self._import_mode = False
            self._start_scan(Path(folder))

    def _start_scan(self, path: Path) -> None:
        self._current_folder = path
        # If a scan is running, cancel it and defer the new scan until the
        # thread has exited — avoids blocking the UI with .wait().
        if self._scanner and self._scanner.isRunning():
            self._scanner.cancel()
            self._scanner.finished.connect(
                lambda: self._begin_scan(path),
                Qt.SingleShotConnection,
            )
            return
        self._begin_scan(path)

    def _begin_scan(self, path: Path) -> None:
        """Unconditionally start a fresh scan of *path* (no running scanner)."""
        # Clear all state
        self._flush_timer.stop()
        self._scan_buffer.clear()
        self._collection.clear()
        self._generator.clear()
        self._file_list.source_model().reset_records([])
        self._grid_view.reset_records([])

        self._statusbar.showMessage("Scanning…")
        self._update_stats()

        self._scanner = ScanWorker(path, recursive=self._recursive)
        self._scanner.file_found.connect(self._on_file_found)
        self._scanner.progress.connect(self._on_scan_progress)
        self._scanner.scan_complete.connect(self._on_scan_complete)
        self._scanner.scan_error.connect(self._on_scan_error)
        self._flush_timer.start()
        self._scanner.start()

    # ------------------------------------------------------------------ #
    # Scanner signal handlers                                              #
    # ------------------------------------------------------------------ #

    def _on_file_found(self, record) -> None:
        self._collection.add(record)
        self._scan_buffer.append(record)

    def _flush_scan_buffer(self) -> None:
        """Push buffered records to both models in one batch insert."""
        if not self._scan_buffer:
            return
        batch = self._scan_buffer[:]
        self._scan_buffer.clear()
        self._file_list.source_model().append_batch(batch)
        self._grid_view.append_batch(batch)
        self._update_stats()

    def _on_scan_progress(self, count: int) -> None:
        self._statusbar.showMessage(f"Scanning… {count} files found")

    def _on_scan_complete(self, count: int) -> None:
        self._flush_timer.stop()
        self._flush_scan_buffer()   # drain any remainder

        if self._import_mode:
            self._import_mode = False
            self._run_import_separation(count)
            return

        pinned_keys = self._load_pair_marks()
        self._collection.build_pairs(pinned_keys=pinned_keys)
        all_records = self._collection.all()
        self._file_list.source_model().reset_records(all_records)
        self._grid_view.reset_records(all_records)
        self._load_prune_marks()

        self._update_stats()
        pairs = self._collection.stats["paired"] // 2
        self._statusbar.showMessage(
            f"Scan complete — {count} file{'s' if count != 1 else ''} found, "
            f"{pairs} pair{'s' if pairs != 1 else ''}"
        )
        self._update_status_count()

    def _run_import_separation(self, scan_count: int) -> None:
        """Separate RAW/JPG after an import scan, then rebuild pairs."""
        from app.ops.separate import SeparationPlan
        self._statusbar.showMessage("Separating RAW and JPG files…")

        all_records = self._collection.all()
        plan = SeparationPlan(all_records)
        succeeded, failed = plan.execute()

        # Snapshot old→new path pairs BEFORE update_path mutates record.path
        moves = [(record, record.path, new_path) for record, new_path in succeeded]
        moved = sum(1 for _, old, new in moves if old != new)

        for record, old_path, new_path in moves:
            self._collection.update_path(old_path, new_path)

        # Auto-detect natural pairs after import sort, then persist them
        self._collection.build_pairs(auto_detect=True)
        self._save_pair_marks()
        all_records = self._collection.all()
        self._file_list.source_model().reset_records(all_records)
        self._grid_view.reset_records(all_records)

        self._update_stats()
        self._update_status_count()
        self._load_prune_marks()

        pairs = self._collection.stats["paired"] // 2
        msg = (
            f"Import complete — {scan_count} file{'s' if scan_count != 1 else ''}, "
            f"{moved} moved, "
            f"{pairs} pair{'s' if pairs != 1 else ''} linked"
        )
        if failed:
            msg += f"  ({len(failed)} move{'s' if len(failed) != 1 else ''} failed)"
        self._statusbar.showMessage(msg)

    def _on_scan_error(self, msg: str) -> None:
        self._statusbar.showMessage(f"Scan error: {msg}")

    # ------------------------------------------------------------------ #
    # Stats helpers                                                        #
    # ------------------------------------------------------------------ #

    def _update_stats(self) -> None:
        s = self._collection.stats
        self._lbl_total.setText(f"{s['total']}  total")
        self._lbl_raw.setText(f"{s['raw']}  RAW")
        self._lbl_jpg.setText(f"{s['jpg']}  JPG")
        self._lbl_paired.setText(f"{s['paired'] // 2}  pairs")
        self._lbl_pruned.setText(f"{s['pruned']}  pruned")

    def _update_status_count(self) -> None:
        """Update the status bar with 'Showing X of Y' for the active view."""
        total = len(self._collection)
        if total == 0:
            return
        current_view = self._stack.currentWidget()
        if hasattr(current_view, "visible_count"):
            shown = current_view.visible_count()
            if shown != total:
                self._statusbar.showMessage(
                    f"Showing {shown} of {total} files (filter active)"
                )
            else:
                self._statusbar.showMessage(f"{total} files")

    # ------------------------------------------------------------------ #
    # Viewer                                                               #
    # ------------------------------------------------------------------ #

    def _on_item_activated(self, record) -> None:
        """Open (or reuse) the image viewer for the activated record."""
        current_view = self._stack.currentWidget()
        records = current_view.all_visible_records()
        index = next(
            (i for i, r in enumerate(records) if r.path == record.path), 0
        )

        if self._viewer is None or not self._viewer.isVisible():
            self._viewer = ImageViewer(
                records, index,
                pair_lookup=self._collection.find_pair,
                parent=self,
            )
            self._viewer.prune_toggled.connect(self._on_viewer_prune_toggled)
            self._viewer.destroyed.connect(lambda: setattr(self, "_viewer", None))
            self._viewer.show()
        else:
            self._viewer.navigate_to(records, index)

        self._viewer.raise_()
        self._viewer.activateWindow()

    # ------------------------------------------------------------------ #
    # Prune                                                                #
    # ------------------------------------------------------------------ #

    def _on_select_mode_toggled(self, checked: bool) -> None:
        """Toolbar Select button toggled — enter or exit multi-select mode."""
        self._grid_view.set_selection_mode(checked)

    def _on_grid_select_mode_changed(self, enabled: bool) -> None:
        """Grid exited select mode via Done/Escape — sync toolbar button."""
        self._act_select.blockSignals(True)
        self._act_select.setChecked(enabled)
        self._act_select.blockSignals(False)

    def _toggle_prune_selected(self) -> None:
        """Cull toolbar button — toggle prune on all selected grid items."""
        records = self._grid_view.selected_records()
        if not records:
            return
        all_pruned = all(r.is_pruned for r in records)
        for r in records:
            r.is_pruned = not all_pruned
        self._refresh_prune(records)

    def _on_prune_toggled(self, records) -> None:
        """Handle prune toggle from list or grid view (receives a list)."""
        self._refresh_prune(records)

    def _on_viewer_prune_toggled(self, record) -> None:
        """Handle prune toggle from the image viewer (receives a single record)."""
        self._refresh_prune([record])

    def _refresh_prune(self, records) -> None:
        """
        Propagate prune state to paired files, then repaint both models.

        If a record has a RAW/JPG pair, the partner inherits the same
        is_pruned value so both are always marked together.
        """
        # Expand the list to include any unpropagated pairs
        all_affected = list(records)
        i = 0
        while i < len(all_affected):
            r = all_affected[i]
            pair = self._collection.find_pair(r)
            if pair is not None and pair.is_pruned != r.is_pruned:
                pair.is_pruned = r.is_pruned
                all_affected.append(pair)
            i += 1

        # Keep the incremental pruned counter in sync.
        # all_affected has already been mutated, so we compare against old state:
        # records passed in had their is_pruned toggled before _refresh_prune was
        # called; pairs were flipped inside the loop above.  Rebuild from ground
        # truth is cheapest here — just recount pruned once.
        self._collection._s.pruned = sum(
            1 for r in self._collection if r.is_pruned
        )

        self._file_list.source_model().notify_records_changed(all_affected)
        self._grid_view.notify_records_changed(all_affected)
        # Re-run the proxy filter so pruned items disappear/appear per current
        # filter bar state (e.g. if "Pruned" toggle is off, hide them immediately)
        self._file_list.filter_proxy().invalidateFilter()
        self._grid_view.apply_filter(self._filter_bar.current_state())
        # Sync viewer button if open and a pair was affected
        if self._viewer and self._viewer.isVisible():
            idx = self._viewer._index
            if 0 <= idx < len(self._viewer._records):
                current = self._viewer._records[idx]
                if any(r.path == current.path for r in all_affected):
                    self._viewer._sync_prune_btn(current)
        self._update_stats()
        self._update_status_count()
        self._save_prune_marks()

    def _open_review(self) -> None:
        pruned = self._collection.pruned()
        if not pruned:
            from PySide6.QtWidgets import QMessageBox
            QMessageBox.information(
                self, "No Files Pruned", "No files are currently marked for pruning."
            )
            return
        dlg = PruneReviewDialog(pruned, parent=self)
        dlg.committed.connect(self._on_trash_committed)
        dlg.all_unmarked.connect(self._unmark_all)
        dlg.exec()

    def _on_trash_committed(self, succeeded) -> None:
        """Remove trashed records from collection and both models."""
        for r in succeeded:
            self._collection.remove(r)
        self._file_list.source_model().remove_records(succeeded)
        self._grid_view.remove_records(succeeded)
        # Close viewer if it's showing a now-deleted record
        if self._viewer and self._viewer.isVisible():
            current = self._viewer._records[self._viewer._index]
            if any(r.path == current.path for r in succeeded):
                self._viewer.close()
        self._update_stats()
        self._update_status_count()
        self._save_prune_marks()
        n = len(succeeded)
        self._statusbar.showMessage(
            f"{n} file{'s' if n != 1 else ''} moved to Trash"
        )

    def _open_sort(self) -> None:
        from PySide6.QtWidgets import QMessageBox
        all_records = self._collection.all()
        if not all_records:
            QMessageBox.information(self, "No Files", "Open a folder first.")
            return
        dlg = SeparateDialog(all_records, parent=self)
        dlg.separated.connect(self._on_sorted)
        dlg.exec()

    def _on_sorted(self, succeeded, failed) -> None:
        """Update paths for moved records, rebuild pairs, and refresh both models."""
        # Capture moved count BEFORE update_path mutates record.path
        moves = [(record, record.path, new_path) for record, new_path in succeeded]
        moved = sum(1 for _, old, new in moves if old != new)

        for record, old_path, new_path in moves:
            self._collection.update_path(old_path, new_path)

        # Rebuild pairs — canonical-parent logic strips RAW/JPG so keys are
        # Reload sidecar to preserve explicit pairs after sort
        pinned_keys = self._load_pair_marks()
        self._collection.build_pairs(pinned_keys=pinned_keys)
        all_records = self._collection.all()
        self._file_list.source_model().reset_records(all_records)
        self._grid_view.reset_records(all_records)
        self._update_stats()
        self._update_status_count()

        pairs = self._collection.stats["paired"] // 2
        msg = f"{moved} file{'s' if moved != 1 else ''} sorted — {pairs} pair{'s' if pairs != 1 else ''} linked"
        if failed:
            msg += f"  ({len(failed)} failed)"
        self._statusbar.showMessage(msg)

    def _open_pair(self) -> None:
        from PySide6.QtWidgets import QMessageBox
        all_records = self._collection.all()
        if not all_records:
            QMessageBox.information(self, "No Files", "Open a folder first.")
            return
        dlg = PairDialog(all_records, parent=self)
        dlg.pairs_saved.connect(self._on_pairs_saved)
        dlg.exec()

    def _on_pairs_saved(self, pair_keys) -> None:
        """Persist pair keys to sidecar, rebuild pairs, and refresh models."""
        self._save_pair_marks(pair_keys)
        self._collection.build_pairs(pinned_keys=pair_keys)
        all_records = self._collection.all()
        self._file_list.source_model().reset_records(all_records)
        self._grid_view.reset_records(all_records)
        self._update_stats()
        self._update_status_count()

        n = len(pair_keys)
        self._statusbar.showMessage(
            f"{n} pair{'s' if n != 1 else ''} saved"
        )

    def _unmark_all(self) -> None:
        records = self._collection.pruned()
        for r in records:
            r.is_pruned = False
        self._refresh_prune(records)
        # Also sync viewer prune button if it's showing a previously-pruned image
        if self._viewer and self._viewer.isVisible():
            idx = self._viewer._index
            if 0 <= idx < len(self._viewer._records):
                self._viewer._sync_prune_btn(self._viewer._records[idx])

    # ------------------------------------------------------------------ #
    # Shortcuts                                                            #
    # ------------------------------------------------------------------ #

    def _show_shortcuts(self) -> None:
        if self._shortcuts_dlg is None or not self._shortcuts_dlg.isVisible():
            self._shortcuts_dlg = KeyboardShortcutsDialog(parent=self)
        self._shortcuts_dlg.show()
        self._shortcuts_dlg.raise_()
        self._shortcuts_dlg.activateWindow()

    # ------------------------------------------------------------------ #
    # Session persistence                                                  #
    # ------------------------------------------------------------------ #

    def _save_prune_marks(self) -> None:
        """Write current prune marks to .photo_pipeline.json in the folder root."""
        if not self._current_folder:
            return
        import json
        pruned = []
        for r in self._collection.pruned():
            try:
                pruned.append(str(r.path.relative_to(self._current_folder)))
            except ValueError:
                pruned.append(str(r.path))   # absolute fallback
        try:
            (self._current_folder / _PRUNE_FILE).write_text(
                json.dumps({"pruned": pruned}, indent=2)
            )
        except Exception:
            pass

    def _load_prune_marks(self) -> None:
        """Apply saved prune marks from .photo_pipeline.json after a scan completes."""
        if not self._current_folder:
            return
        import json
        sidecar = self._current_folder / _PRUNE_FILE
        if not sidecar.exists():
            return
        try:
            data = json.loads(sidecar.read_text())
            pruned_paths = set()
            for rel in data.get("pruned", []):
                p = Path(rel)
                pruned_paths.add(p if p.is_absolute() else self._current_folder / p)
        except Exception:
            return

        affected = [r for r in self._collection.all() if r.path in pruned_paths]
        for r in affected:
            r.is_pruned = True
        if affected:
            self._collection._s.pruned = sum(
                1 for r in self._collection if r.is_pruned
            )
            self._file_list.source_model().notify_records_changed(affected)
            self._grid_view.notify_records_changed(affected)
            self._update_stats()
            self._update_status_count()

    def _save_pair_marks(self, pair_keys=None) -> None:
        """Write pair keys to sidecar.  If pair_keys is None, use current collection."""
        if not self._current_folder:
            return
        from app.models.sidecar import write_paired_keys
        keys = pair_keys if pair_keys is not None else self._collection.current_pair_keys()
        write_paired_keys(self._current_folder, keys)

    def _load_pair_marks(self):
        """Read pair keys from sidecar.  Returns set or empty set."""
        if not self._current_folder:
            return set()
        from app.models.sidecar import read_paired_keys
        return read_paired_keys(self._current_folder)

    def _save_session(self) -> None:
        from datetime import date as _pydate
        s = QSettings(_SETTINGS_ORG, _SETTINGS_APP)
        s.setValue("geometry", self.saveGeometry())
        s.setValue("window_state", self.saveState())
        s.setValue("view_index", self._stack.currentIndex())
        state = self._filter_bar.current_state()
        s.setValue("filter/pruned", state.show_pruned)
        s.setValue("sort/key", state.sort_key)
        s.setValue("sort/asc", state.sort_asc)
        s.setValue("filter/date_from",
                   state.date_from.isoformat() if state.date_from else "")
        s.setValue("filter/date_to",
                   state.date_to.isoformat()   if state.date_to   else "")
        s.setValue("last_folder",
                   str(self._current_folder) if self._current_folder else "")

    def _restore_session(self) -> None:
        from datetime import date as _pydate
        s = QSettings(_SETTINGS_ORG, _SETTINGS_APP)
        if geom := s.value("geometry"):
            self.restoreGeometry(geom)
        if state := s.value("window_state"):
            self.restoreState(state)
        self._toolbar.setVisible(True)   # never let saved state hide the toolbar
        view_idx = s.value("view_index", _VIEW_GRID, type=int)
        self._switch_view(view_idx)

        def _parse_date(val: str):
            try:
                return _pydate.fromisoformat(val) if val else None
            except ValueError:
                return None

        self._filter_bar.restore_state(
            show_pruned = s.value("filter/pruned", False, type=bool),
            sort_key    = s.value("sort/key", "date", type=str),
            sort_asc    = s.value("sort/asc", True,   type=bool),
            date_from   = _parse_date(s.value("filter/date_from", "", type=str)),
            date_to     = _parse_date(s.value("filter/date_to",   "", type=str)),
        )

        last = s.value("last_folder", "", type=str)
        if last:
            p = Path(last)
            if p.exists() and p.is_dir():
                self._import_mode = False
                self._start_scan(p)

    # ------------------------------------------------------------------ #
    # Close                                                                #
    # ------------------------------------------------------------------ #

    def closeEvent(self, event) -> None:
        self._save_session()
        if self._scanner and self._scanner.isRunning():
            self._scanner.cancel()
            # Don't block — scanner holds no resources that need ordered cleanup.
            # It will finish in the background after the process exits.
        self._generator.shutdown()
        super().closeEvent(event)
