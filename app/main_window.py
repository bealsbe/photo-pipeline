"""
MainWindow — top-level application window.
"""
from __future__ import annotations

from pathlib import Path

from typing import List

from PySide6.QtCore import QDir, QSettings, QTimer, Qt
from PySide6.QtGui import QAction, QActionGroup
from PySide6.QtWidgets import (
    QFileDialog,
    QHBoxLayout,
    QLabel,
    QMainWindow,
    QStackedWidget,
    QStatusBar,
    QVBoxLayout,
    QWidget,
)

from app.models.collection import PhotoCollection
from app.models.sidecar import atomic_write_text
from app.models.photo_record import FileType
from app.scanning.scanner import ScanWorker
from app.thumbnails.generator import ThumbnailGenerator
from app.ui.calendar_view import CalendarView
from app.ui.file_list import FileListWidget
from app.ui.filter_bar import FilterBar
from app.ui.folder_bar import FolderBar
from app.ui.icons import calendar_icon, check_icon, icon, tinted_icon
from app.ui.prune_review import PruneReviewDialog
from app.ui.pair_dialog import PairDialog
from app.ui.separate_dialog import SeparateDialog
from app.ui.shortcuts_dialog import KeyboardShortcutsDialog
from app.ui.side_menu import SideMenu
from app.ui.grouped_grid import GroupedGridView
from app.ui.viewer import ImageViewer

_SETTINGS_ORG = "PhotoPipeline"
_SETTINGS_APP = "PhotoPipeline"
_PRUNE_FILE   = ".photo_pipeline.json"       # sidecar: prune marks
_PAIRS_FILE   = ".photo-pipeline-pairs.json" # sidecar: persistent pair marks
_EXPORT_FILE  = ".photo-pipeline-export.json"  # sidecar: marked-for-export marks
_CROPS_FILE   = ".photo-pipeline-crops.json"   # sidecar: per-file crop rects



_THUMB_SIZE      = 180   # default display size
_GENERATION_SIZE = 300   # fixed thumbnail generation + cache size
_ZOOM_MIN        = 120
_ZOOM_MAX        = 480
_ZOOM_STEP       = 20
_VIEW_LIST     = 0
_VIEW_GRID     = 1
_VIEW_CALENDAR = 2


class MainWindow(QMainWindow):
    def __init__(self) -> None:
        super().__init__()
        self.setWindowTitle("Photo Pipeline")
        self.setWindowIcon(tinted_icon("retro-camera", "#ff6d00", size=48))
        self.resize(1200, 780)

        self._collection = PhotoCollection()
        self._scanner: ScanWorker | None = None
        self._generator = ThumbnailGenerator(thumb_size=_GENERATION_SIZE, parent=self)
        self._viewer: ImageViewer | None = None
        self._current_folder: Path | None = None
        self._library_folder: Path | None = None

        # Scan buffer: accumulate scanned records and flush on a timer, so the
        # models see one insert per frame rather than one per scanner batch.
        self._scan_buffer: List = []
        self._flush_timer = QTimer(self)
        self._flush_timer.setInterval(150)   # flush every 150 ms while scanning
        self._flush_timer.timeout.connect(self._flush_scan_buffer)

        self._shortcuts_dlg: KeyboardShortcutsDialog | None = None

        self._build_actions()
        self._build_central()
        self._build_side_menu()
        self._build_menu()
        self._build_statusbar()
        # Keep filter-bar zoom slider in sync with Ctrl+scroll / pinch
        self._grid_view.thumb_size_changed.connect(self._filter_bar.set_zoom)
        self._filter_bar.zoom_changed.connect(self._on_thumb_size_changed)
        self._restore_session()

    # ------------------------------------------------------------------ #
    # Construction                                                         #
    # ------------------------------------------------------------------ #

    def _build_actions(self) -> None:
        """
        One QAction per command, shared by the menu bar and the side menu.

        Each shortcut must be bound exactly once: when a menu item and a
        toolbar button each carried their own copy, Qt saw an ambiguous
        shortcut and fired neither — Ctrl+1/2/3, Ctrl+O and friends were dead.
        """
        def act(icon_, text, slot=None, shortcut=None, tip=None, status=None,
                checkable=False):
            a = QAction(icon_, text, self)
            if shortcut:
                a.setShortcut(shortcut)
            if tip:
                a.setToolTip(tip)
            if status:
                a.setStatusTip(status)
            a.setCheckable(checkable)
            if slot is not None:
                a.triggered.connect(slot)
            return a

        self._act_open = act(
            icon("folder-open"), "&Open Working Folder…", self.open_folder, "Ctrl+O",
            "Open working folder  (Ctrl+O)", "Open a folder to browse and cull")
        self._act_export = act(
            icon("file-import"), "&Export to Library…", self._open_export, "Ctrl+E",
            "Export to library  (Ctrl+E)", "Export working folder to the library structure")
        self._act_edit_copy = act(
            tinted_icon("folder-open", "#5a8acc", size=24), "&Copy Marked RAWs to Folder…",
            self._open_edit_copy, "Ctrl+Shift+E",
            "Copy the RAW of every marked shot into a folder to edit from  (Ctrl+Shift+E)",
            "Copy marked RAW files into one folder for editing")
        self._act_review = act(
            icon("trash"), "&Review Pruned…", self._open_review, "Ctrl+R",
            "Review pruned files → Trash  (Ctrl+R)", "Review pruned files and commit to Trash")
        self._act_sort = act(
            icon("fork"), "&Sort into RAW / JPG…", self._open_sort, "Ctrl+Shift+S",
            "Sort into RAW/ and JPG/ subfolders  (Ctrl+Shift+S)",
            "Sort files into RAW/ and JPG/ subfolders")
        self._act_pair = act(
            icon("link"), "&Pair RAW + JPG…", self._open_pair, "Ctrl+Shift+P",
            "Pair RAW+JPG by stem and save  (Ctrl+Shift+P)",
            "Pair RAW and JPG files by stem and save to sidecar")

        self._act_list = act(
            icon("bullet-list"), "&List View", lambda: self._switch_view(_VIEW_LIST),
            "Ctrl+1", "List view  (Ctrl+1)", checkable=True)
        self._act_grid = act(
            icon("grid"), "&Grid View", lambda: self._switch_view(_VIEW_GRID),
            "Ctrl+2", "Grid view  (Ctrl+2)", checkable=True)
        self._act_calendar = act(
            calendar_icon("#c8c8d8", 24), "Library &Calendar",
            lambda: self._switch_view(_VIEW_CALENDAR),
            "Ctrl+3", "Library calendar  (Ctrl+3)", checkable=True)
        self._act_list.setChecked(True)
        grp = QActionGroup(self)
        grp.setExclusive(True)
        for a in (self._act_list, self._act_grid, self._act_calendar):
            grp.addAction(a)

        self._act_select = act(
            icon("plus"), "&Select Mode", None, "S", "Multi-select mode  (S)",
            checkable=True)
        self._act_select.toggled.connect(self._on_select_mode_toggled)
        # P and M are handled by the grid / list themselves; these buttons act
        # on the current selection and carry no shortcut of their own.
        self._act_cull = act(
            icon("times-circle"), "&Prune Selected", self._toggle_prune_selected,
            None, "Mark selected as pruned  (P)")
        self._act_mark = act(
            check_icon("#3ac96a", size=24), "&Mark Selected for Export",
            self._toggle_mark_selected, None, "Mark selected for export  (M)")

    def _build_menu(self) -> None:
        mb = self.menuBar()

        file_menu = mb.addMenu("&File")
        file_menu.addAction(self._act_open)
        file_menu.addSeparator()
        quit_act = QAction("&Quit", self)
        quit_act.setShortcut("Ctrl+Q")
        quit_act.triggered.connect(self.close)
        file_menu.addAction(quit_act)

        prune_menu = mb.addMenu("&Prune")
        prune_menu.addAction(self._act_review)
        prune_menu.addAction(self._act_sort)
        prune_menu.addAction(self._act_pair)
        prune_menu.addSeparator()
        prune_menu.addAction(self._act_export)
        prune_menu.addAction(self._act_edit_copy)

        help_menu = mb.addMenu("&Help")
        shortcuts_act = QAction("&Keyboard Shortcuts…", self)
        shortcuts_act.setShortcut("?")
        shortcuts_act.triggered.connect(self._show_shortcuts)
        help_menu.addAction(shortcuts_act)

        view_menu = mb.addMenu("&View")
        view_menu.addAction(self._act_list)
        view_menu.addAction(self._act_grid)
        view_menu.addAction(self._act_calendar)
        view_menu.addSeparator()

        side_act = QAction("Side &Menu Labels", self)
        side_act.setShortcut("Ctrl+\\")
        side_act.setStatusTip("Expand the side menu to show command names")
        side_act.triggered.connect(self._side_menu.toggle)
        view_menu.addAction(side_act)

        self._act_folder_bar = QAction("&Folder Bar", self)
        self._act_folder_bar.setShortcut("Ctrl+B")
        self._act_folder_bar.setCheckable(True)
        self._act_folder_bar.setChecked(True)
        self._act_folder_bar.setStatusTip("Show/hide the working and library folder bar")
        self._act_folder_bar.toggled.connect(self._on_folder_bar_toggled)
        view_menu.addAction(self._act_folder_bar)

    def _build_side_menu(self) -> None:
        """Populate the collapsible rail on the left (built in _build_central)."""
        m = self._side_menu
        m.add_action(self._act_open)
        m.add_action(self._act_export)
        m.add_action(self._act_edit_copy)
        m.add_separator()
        m.add_action(self._act_review)
        m.add_action(self._act_sort)
        m.add_action(self._act_pair)
        m.add_separator()
        m.add_action(self._act_list)
        m.add_action(self._act_grid)
        m.add_action(self._act_calendar)
        m.add_separator()
        m.add_action(self._act_select)
        m.add_action(self._act_cull, hint="P")
        m.add_action(self._act_mark, hint="M")

    def _build_central(self) -> None:
        root = QWidget()
        self.setCentralWidget(root)
        root_lay = QHBoxLayout(root)
        root_lay.setContentsMargins(0, 0, 0, 0)
        root_lay.setSpacing(0)

        # ── side menu: a fixed rail slot; the panel floats over `root` ── #
        self._side_menu = SideMenu(root)
        root_lay.addWidget(self._side_menu.placeholder())

        column = QWidget()
        root_lay.addWidget(column, 1)
        layout = QVBoxLayout(column)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(0)

        # ── folder bar ────────────────────────────────────────────── #
        self._folder_bar = FolderBar()
        layout.addWidget(self._folder_bar)
        self._folder_bar.working_folder_requested.connect(self.open_folder)
        self._folder_bar.library_folder_changed.connect(self._on_library_folder_changed)
        self._folder_bar.collapsed_changed.connect(self._on_folder_bar_collapsed)

        # ── inner widget holds filter bar + views ─────────────────── #
        inner = QWidget()
        layout.addWidget(inner, 1)
        inner_layout = QVBoxLayout(inner)
        inner_layout.setContentsMargins(0, 0, 0, 0)
        inner_layout.setSpacing(0)

        # ── filter bar ────────────────────────────────────────────── #
        self._filter_bar = FilterBar()
        inner_layout.addWidget(self._filter_bar)
        self._filter_bar.filter_changed.connect(self._on_filter_changed)

        # ── stacked views ─────────────────────────────────────────── #
        self._stack = QStackedWidget()
        inner_layout.addWidget(self._stack)

        self._file_list = FileListWidget()
        self._grid_view = GroupedGridView(self._generator, thumb_size=_THUMB_SIZE)

        self._calendar  = CalendarView()

        self._stack.addWidget(self._file_list)   # index 0 → list
        self._stack.addWidget(self._grid_view)   # index 1 → grid
        self._stack.addWidget(self._calendar)    # index 2 → library calendar

        self._calendar.item_activated.connect(self._on_calendar_item_activated)
        self._calendar.open_folder_requested.connect(self._on_calendar_open_folder)
        self._calendar.library_folder_requested.connect(self._folder_bar.pick_library)
        self._calendar.status_changed.connect(self._on_calendar_status)

        self._file_list.item_activated.connect(self._on_item_activated)
        self._grid_view.item_activated.connect(self._on_item_activated)

        self._file_list.prune_toggled.connect(self._on_prune_toggled)
        self._grid_view.prune_toggled.connect(self._on_prune_toggled)

        self._file_list.mark_toggled.connect(self._on_mark_toggled)
        self._grid_view.mark_toggled.connect(self._on_mark_toggled)

        # Keep toolbar Select button in sync when mode exits via Done/Escape
        self._grid_view.selection_mode_changed.connect(self._on_grid_select_mode_changed)

        # Siblings stack in creation order and the content column came after
        # the side menu — lift the floating panel back above it.
        self._side_menu.raise_()

    def _build_statusbar(self) -> None:
        self._statusbar = QStatusBar()
        self._statusbar.setStyleSheet(
            "QStatusBar {"
            "  background: #14142a;"
            "  border-top: 1px solid rgba(255,109,0,0.15);"
            "  color: #7878a0; font-size: 11px;"
            "}"
            "QStatusBar::item { border: none; }"
        )
        self.setStatusBar(self._statusbar)

        # ── stats chips (permanent — right-aligned) ───────────────── #
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
        self._lbl_marked = _chip("#0d201a", "#3ac96a")

        for lbl in (self._lbl_total, self._lbl_raw, self._lbl_jpg,
                    self._lbl_paired, self._lbl_pruned, self._lbl_marked):
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
        if idx not in (_VIEW_LIST, _VIEW_GRID, _VIEW_CALENDAR):
            idx = _VIEW_GRID
        self._stack.setCurrentIndex(idx)
        self._act_list.setChecked(idx == _VIEW_LIST)
        self._act_grid.setChecked(idx == _VIEW_GRID)
        self._act_calendar.setChecked(idx == _VIEW_CALENDAR)
        self._filter_bar.set_zoom_enabled(idx == _VIEW_GRID)

        # The calendar browses the library, not the working folder: the filter
        # bar and the culling actions don't apply to it.
        on_calendar = idx == _VIEW_CALENDAR
        self._filter_bar.setVisible(not on_calendar)
        for act in (self._act_select, self._act_cull, self._act_mark):
            act.setEnabled(not on_calendar)
        if on_calendar:
            self._calendar.activate()
        else:
            self._calendar.deactivate()
            if len(self._collection) == 0:
                # Don't leave the calendar's library summary behind
                self._statusbar.showMessage("Ready — open a folder to begin")
            self._update_status_count()

    def _on_thumb_size_changed(self, size: int) -> None:
        snapped = max(_ZOOM_MIN, min(_ZOOM_MAX, (size // _ZOOM_STEP) * _ZOOM_STEP))
        # Don't touch generator.thumb_size — it stays fixed at _GENERATION_SIZE
        # so the disk cache is always valid regardless of display zoom.
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

    def open_folder(self) -> None:
        folder = QFileDialog.getExistingDirectory(
            self,
            "Open Working Folder",
            str(self._current_folder or QDir.homePath()),
            QFileDialog.ShowDirsOnly | QFileDialog.DontResolveSymlinks,
        )
        if folder:
            self._start_scan(Path(folder))

    def _start_scan(self, path: Path) -> None:
        self._current_folder = path
        self._folder_bar.set_working_folder(path)
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

        self._scanner = ScanWorker(path, recursive=True)
        self._scanner.files_found.connect(self._on_files_found)
        self._scanner.progress.connect(self._on_scan_progress)
        self._scanner.scan_complete.connect(self._on_scan_complete)
        self._scanner.scan_error.connect(self._on_scan_error)
        self._flush_timer.start()
        self._scanner.start()

    # ------------------------------------------------------------------ #
    # Scanner signal handlers                                              #
    # ------------------------------------------------------------------ #

    def _on_files_found(self, records) -> None:
        for record in records:
            self._collection.add(record)
        self._scan_buffer.extend(records)

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

        pinned_keys = self._load_pair_marks()
        self._collection.build_pairs(pinned_keys=pinned_keys)
        all_records = self._collection.all()
        self._file_list.source_model().reset_records(all_records)
        self._grid_view.reset_records(all_records)
        self._load_prune_marks()
        self._load_export_marks()
        self._load_crops()

        self._update_stats()
        pairs = self._collection.stats["paired"] // 2
        self._statusbar.showMessage(
            f"Scan complete — {count} file{'s' if count != 1 else ''} found, "
            f"{pairs} pair{'s' if pairs != 1 else ''}"
        )
        self._update_status_count()

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
        self._lbl_marked.setText(f"{s['marked']}  marked")

    def _update_status_count(self) -> None:
        """Update the status bar with 'Showing X of Y' for the active view."""
        if self._stack.currentIndex() == _VIEW_CALENDAR:
            return
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
        self._open_viewer(record, records, self._collection.find_pair, read_only=False)

    def _on_calendar_item_activated(self, record, records, pair_lookup) -> None:
        """A library photo was opened from the calendar's day view."""
        self._open_viewer(record, records, pair_lookup, read_only=True)

    def _open_viewer(self, record, records, pair_lookup, read_only: bool) -> None:
        index = next(
            (i for i, r in enumerate(records) if r.path == record.path), 0
        )

        viewer = self._viewer
        if viewer is not None and viewer.isVisible() and viewer.is_read_only != read_only:
            # A culling viewer and a library viewer differ in their controls;
            # swap rather than repurpose.
            viewer.close()
            viewer = self._viewer = None

        if viewer is None or not viewer.isVisible():
            self._viewer = ImageViewer(
                records, index,
                pair_lookup=pair_lookup,
                parent=self,
                read_only=read_only,
            )
            self._viewer.prune_toggled.connect(self._on_viewer_prune_toggled)
            self._viewer.mark_toggled.connect(self._on_viewer_mark_toggled)
            self._viewer.crop_changed.connect(self._on_viewer_crop_changed)
            v = self._viewer
            v.destroyed.connect(
                lambda: setattr(self, "_viewer", None) if self._viewer is v else None
            )
            self._viewer.show()
        else:
            self._viewer.navigate_to(records, index, pair_lookup=pair_lookup)

        self._viewer.raise_()
        self._viewer.activateWindow()

    def _on_calendar_open_folder(self, path: Path) -> None:
        """'Open as working folder' on a library day."""
        self._start_scan(path)
        self._switch_view(_VIEW_GRID)

    def _on_calendar_status(self, text: str) -> None:
        if self._stack.currentIndex() == _VIEW_CALENDAR:
            self._statusbar.showMessage(text)

    # ------------------------------------------------------------------ #
    # Prune                                                                #
    # ------------------------------------------------------------------ #

    def _on_folder_bar_toggled(self, visible: bool) -> None:
        self._folder_bar.setVisible(visible)

    def _on_folder_bar_collapsed(self, collapsed: bool) -> None:
        from PySide6.QtCore import QSettings
        QSettings(_SETTINGS_ORG, _SETTINGS_APP).setValue("folder_bar_collapsed", collapsed)

    def _on_library_folder_changed(self, path: Path) -> None:
        self._library_folder = path
        self._folder_bar.set_library_folder(path)
        self._calendar.set_library_root(path)

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

    def _toggle_mark_selected(self) -> None:
        """Mark toolbar button — toggle mark-for-export on all selected grid items."""
        records = self._grid_view.selected_records()
        if not records:
            return
        all_marked = all(r.is_marked_for_export for r in records)
        for r in records:
            r.is_marked_for_export = not all_marked
        self._refresh_mark(records)

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

    def _on_mark_toggled(self, records) -> None:
        """Handle mark-for-export toggle from list or grid view (receives a list)."""
        self._refresh_mark(records)

    def _on_viewer_mark_toggled(self, record) -> None:
        """Handle mark toggle from the image viewer (receives a single record)."""
        self._refresh_mark([record])

    def _on_export_marks_cleared(self, cleared_records) -> None:
        """ExportDialog cleared all marks — refresh views + sidecar."""
        self._refresh_mark(cleared_records)

    def _refresh_mark(self, records) -> None:
        """Propagate mark state to paired files, repaint models, save sidecar."""
        all_affected = list(records)
        i = 0
        while i < len(all_affected):
            r = all_affected[i]
            pair = self._collection.find_pair(r)
            if pair is not None and pair.is_marked_for_export != r.is_marked_for_export:
                pair.is_marked_for_export = r.is_marked_for_export
                all_affected.append(pair)
            i += 1

        self._collection._s.marked = sum(
            1 for r in self._collection if r.is_marked_for_export
        )

        self._file_list.source_model().notify_records_changed(all_affected)
        self._grid_view.notify_records_changed(all_affected)
        if self._viewer and self._viewer.isVisible():
            idx = self._viewer._index
            if 0 <= idx < len(self._viewer._records):
                current = self._viewer._records[idx]
                if any(r.path == current.path for r in all_affected):
                    self._viewer._sync_mark_btn(current)
        self._update_stats()
        self._update_status_count()
        self._save_export_marks()

    # ------------------------------------------------------------------ #
    # Crop                                                                 #
    # ------------------------------------------------------------------ #

    def _on_viewer_crop_changed(self, record) -> None:
        """A crop was applied or cleared in the viewer."""
        self._refresh_crop([record])

    def _refresh_crop(self, records) -> None:
        """
        Copy the crop onto each record's pair, repaint, and persist.

        A RAW and its JPG are two renderings of one exposure, so a crop made
        on either is a statement about the shot, not about the file — the same
        reasoning that already propagates prune and keep marks across a pair.
        """
        all_affected = list(records)
        i = 0
        while i < len(all_affected):
            r = all_affected[i]
            pair = self._collection.find_pair(r)
            if pair is not None and pair.crop != r.crop:
                pair.crop = r.crop
                all_affected.append(pair)
            i += 1

        self._file_list.source_model().notify_records_changed(all_affected)
        self._grid_view.notify_records_changed(all_affected)
        self._save_crops()

        n = sum(1 for r in self._collection if r.is_cropped)
        touched = all_affected[0] if all_affected else None
        verb = "Crop cleared" if (touched and not touched.is_cropped) else "Crop saved"
        self._statusbar.showMessage(
            f"{verb} — {n} file{'s' if n != 1 else ''} cropped in this folder", 4000
        )

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

        # The prune / export / crop sidecars are keyed by file path, and the
        # sort just moved every file into RAW/ or JPG/.  Without rewriting
        # them here they still name the pre-sort paths, and every mark the
        # user made is silently dropped the next time the folder is opened.
        # (The pair sidecar is immune — its keys use the canonical parent,
        # which deliberately strips the RAW/JPG level.)
        self._save_prune_marks()
        self._save_export_marks()
        self._save_crops()

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

    def _open_export(self) -> None:
        from PySide6.QtWidgets import QMessageBox
        from app.ui.export_dialog import ExportDialog
        all_records = self._collection.all()
        if not all_records:
            QMessageBox.information(self, "No Files", "Open a folder first.")
            return

        # If no library folder set, the dialog handles it inline.
        dlg = ExportDialog(
            records        = all_records,
            working_folder = self._current_folder,
            library_folder = self._library_folder,
            pair_lookup    = self._collection.find_pair,
            parent         = self,
        )
        dlg.library_folder_changed.connect(self._on_library_folder_changed)
        dlg.library_folder_changed.connect(self._folder_bar.set_library_folder)
        dlg.exported.connect(self._on_exported)
        dlg.marks_cleared.connect(self._on_export_marks_cleared)
        dlg.exec()

    def _open_edit_copy(self) -> None:
        """Copy the RAW of every marked shot into a folder to edit from."""
        from PySide6.QtWidgets import QMessageBox
        from app.ops.edit_copy import select_marked_raws
        from app.ui.edit_copy_dialog import EditCopyDialog

        if not len(self._collection):
            QMessageBox.information(self, "No Files", "Open a folder first.")
            return
        selection = select_marked_raws(
            self._collection.all(), self._collection.find_pair,
            self._collection.stem_siblings,
        )
        if not selection.raws and not selection.jpg_only:
            QMessageBox.information(
                self, "Nothing Marked",
                "No shots are marked.\n\nSelect photos and press M (or Keep in the "
                "viewer) to mark the ones you want to edit.",
            )
            return

        s = QSettings(_SETTINGS_ORG, _SETTINGS_APP)
        last = s.value("edit_copy/folder", "", type=str)
        dest = Path(last) if last and Path(last).is_dir() else None
        dlg = EditCopyDialog(
            selection,
            working_folder = self._current_folder,
            dest_folder    = dest,
            use_subfolder  = s.value("edit_copy/subfolder", True, type=bool),
            parent         = self,
        )
        dlg.folder_changed.connect(self._on_edit_copy_folder_changed)
        dlg.copied.connect(self._on_edit_copied)
        dlg.exec()

    def _on_edit_copy_folder_changed(self, folder: Path, subfolder: bool) -> None:
        s = QSettings(_SETTINGS_ORG, _SETTINGS_APP)
        s.setValue("edit_copy/folder", str(folder))
        s.setValue("edit_copy/subfolder", subfolder)

    def _on_edit_copied(self, copied: list, failed: list) -> None:
        n = len(copied)
        msg = f"{n} RAW file{'s' if n != 1 else ''} copied for editing"
        if failed:
            msg += f"  ({len(failed)} failed)"
        self._statusbar.showMessage(msg)

    def _on_exported(self, succeeded: list, failed: list) -> None:
        if succeeded:
            self._calendar.mark_stale()
        n = len(succeeded)
        msg = f"{n} file{'s' if n != 1 else ''} exported to library"
        if failed:
            msg += f"  ({len(failed)} failed)"
        self._statusbar.showMessage(msg)

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
            atomic_write_text(
                self._current_folder / _PRUNE_FILE,
                json.dumps({"pruned": pruned}, indent=2),
            )
        except Exception as exc:
            self._statusbar.showMessage(
                f"\u26a0 Could not save prune marks: {exc}", 8000
            )

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

    def _save_export_marks(self) -> None:
        """Write current export marks to .photo-pipeline-export.json."""
        if not self._current_folder:
            return
        import json
        marked = []
        for r in self._collection.marked():
            try:
                marked.append(str(r.path.relative_to(self._current_folder)))
            except ValueError:
                marked.append(str(r.path))
        try:
            atomic_write_text(
                self._current_folder / _EXPORT_FILE,
                json.dumps({"marked": marked}, indent=2),
            )
        except Exception as exc:
            self._statusbar.showMessage(
                f"⚠ Could not save export marks: {exc}", 8000
            )

    def _load_export_marks(self) -> None:
        """Apply saved export marks from .photo-pipeline-export.json after a scan."""
        if not self._current_folder:
            return
        import json
        sidecar = self._current_folder / _EXPORT_FILE
        if not sidecar.exists():
            return
        try:
            data = json.loads(sidecar.read_text())
            marked_paths = set()
            for rel in data.get("marked", []):
                p = Path(rel)
                marked_paths.add(p if p.is_absolute() else self._current_folder / p)
        except Exception:
            return

        affected = [r for r in self._collection.all() if r.path in marked_paths]
        for r in affected:
            r.is_marked_for_export = True
        if affected:
            self._collection._s.marked = sum(
                1 for r in self._collection if r.is_marked_for_export
            )
            self._file_list.source_model().notify_records_changed(affected)
            self._grid_view.notify_records_changed(affected)
            self._update_stats()
            self._update_status_count()

    def _save_crops(self) -> None:
        """Write current crop rects to .photo-pipeline-crops.json."""
        if not self._current_folder:
            return
        from app.models.crop_sidecar import write_crops
        crops = {r.path: r.crop for r in self._collection if r.is_cropped}
        try:
            write_crops(self._current_folder, crops)
        except Exception as exc:
            self._statusbar.showMessage(
                f"\u26a0 Could not save crops: {exc}", 8000
            )

    def _load_crops(self) -> None:
        """Apply saved crop rects from .photo-pipeline-crops.json after a scan."""
        if not self._current_folder:
            return
        from app.models.crop_sidecar import read_crops
        saved = read_crops(self._current_folder)
        if not saved:
            return

        affected = []
        for r in self._collection.all():
            crop = saved.get(r.path)
            if crop is not None:
                r.crop = crop
                affected.append(r)
        if affected:
            self._file_list.source_model().notify_records_changed(affected)
            self._grid_view.notify_records_changed(affected)

    def _save_pair_marks(self, pair_keys=None) -> None:
        """Write pair keys to sidecar.  If pair_keys is None, use current collection."""
        if not self._current_folder:
            return
        from app.models.sidecar import write_paired_keys
        keys = pair_keys if pair_keys is not None else self._collection.current_pair_keys()
        try:
            write_paired_keys(self._current_folder, keys)
        except Exception as exc:
            self._statusbar.showMessage(
                f"\u26a0 Could not save pair marks: {exc}", 8000
            )

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
        s.setValue("filter/marked_only", not state.show_unmarked)
        s.setValue("sort/key", state.sort_key)
        s.setValue("sort/asc", state.sort_asc)
        s.setValue("filter/date_from",
                   state.date_from.isoformat() if state.date_from else "")
        s.setValue("filter/date_to",
                   state.date_to.isoformat()   if state.date_to   else "")
        s.setValue("last_folder",
                   str(self._current_folder) if self._current_folder else "")
        s.setValue("library_folder",
                   str(self._library_folder) if self._library_folder else "")
        s.setValue("folder_bar_visible", self._act_folder_bar.isChecked())
        for key, value in self._calendar.save_state().items():
            s.setValue(f"calendar/{key}", value)

    def _restore_session(self) -> None:
        from datetime import date as _pydate
        s = QSettings(_SETTINGS_ORG, _SETTINGS_APP)
        if geom := s.value("geometry"):
            self.restoreGeometry(geom)
        if state := s.value("window_state"):
            self.restoreState(state)
        self._calendar.restore_state({
            "mode":  s.value("calendar/mode", "", type=str),
            "year":  s.value("calendar/year", 0, type=int),
            "month": s.value("calendar/month", 0, type=int),
        })
        view_idx = s.value("view_index", _VIEW_GRID, type=int)

        def _parse_date(val: str):
            try:
                return _pydate.fromisoformat(val) if val else None
            except ValueError:
                return None

        self._filter_bar.restore_state(
            show_pruned = s.value("filter/pruned", False, type=bool),
            marked_only = s.value("filter/marked_only", False, type=bool),
            sort_key    = s.value("sort/key", "date", type=str),
            sort_asc    = s.value("sort/asc", True,   type=bool),
            date_from   = _parse_date(s.value("filter/date_from", "", type=str)),
            date_to     = _parse_date(s.value("filter/date_to",   "", type=str)),
        )

        # Restore library folder
        lib = s.value("library_folder", "", type=str)
        if lib:
            p = Path(lib)
            if p.exists() and p.is_dir():
                self._library_folder = p
                self._folder_bar.set_library_folder(p)
                self._calendar.set_library_root(p)

        # Switch view only now, so the calendar knows its library when shown
        self._switch_view(view_idx)

        # Restore folder bar visibility + collapsed state
        bar_visible = s.value("folder_bar_visible", True, type=bool)
        self._act_folder_bar.setChecked(bar_visible)
        self._folder_bar.setVisible(bar_visible)
        bar_collapsed = s.value("folder_bar_collapsed", False, type=bool)
        self._folder_bar.set_collapsed(bar_collapsed)

        # Restore last working folder
        last = s.value("last_folder", "", type=str)
        if last:
            p = Path(last)
            if p.exists() and p.is_dir():
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
        self._calendar.shutdown()
        super().closeEvent(event)
