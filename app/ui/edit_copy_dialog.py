"""
EditCopyDialog — copy the RAW of every marked shot into one folder to edit from.

Layout
------
  Header  — title + one-line explanation
  Card    — FROM (working folder) / TO (chosen folder, "change") and the
            "put them in a subfolder named after the shoot" option
  Chips   — what will happen: files + size, already there, renamed, cropped,
            matched by name, JPG-only, pruned
  List    — every file and where it goes
  Footer  — progress while copying; Copy / Cancel, then Open Folder / Close

The copy runs on a QThread and can be cancelled between files.  All the
decisions (which files, which names) live in app/ops/edit_copy.py.
"""
from __future__ import annotations

import subprocess
from pathlib import Path
from typing import List, Optional

from PySide6.QtCore import QDir, QThread, Qt, Signal
from PySide6.QtGui import QColor
from PySide6.QtWidgets import (
    QCheckBox,
    QDialog,
    QFileDialog,
    QFrame,
    QHBoxLayout,
    QLabel,
    QListWidget,
    QListWidgetItem,
    QProgressBar,
    QPushButton,
    QSizePolicy,
    QVBoxLayout,
    QWidget,
)

from app.ops.edit_copy import EditCopyPlan, MarkedRaws

_BG_DIALOG = "#0c0c18"
_BG_CARD   = "#12121f"
_BORDER    = "rgba(255,109,0,0.18)"
_TEXT_PRI  = "#d0d0e8"
_TEXT_SEC  = "#7878a0"
_TEXT_DIM  = "#3a3a58"

_CHECK_QSS = (
    f"QCheckBox {{ color:{_TEXT_SEC}; font-size:12px; spacing:8px; }}"
    "QCheckBox::indicator { width:15px; height:15px; border-radius:3px;"
    " border:1px solid rgba(255,109,0,0.28); background:#0a0a14; }"
    "QCheckBox::indicator:hover { border-color:rgba(255,109,0,0.55); }"
    "QCheckBox::indicator:checked { background:rgba(255,109,0,0.35);"
    " border-color:rgba(255,109,0,0.70); }"
)
_PRIMARY_QSS = (
    "QPushButton { background:rgba(255,109,0,0.22); color:#ff6d00;"
    " border:1px solid rgba(255,109,0,0.60); border-radius:6px;"
    " font-size:12px; font-weight:700; padding:8px 20px; }"
    "QPushButton:hover { background:rgba(255,109,0,0.34); }"
    "QPushButton:disabled { background:#12121f; color:#3a3a58;"
    " border-color:rgba(255,255,255,0.06); }"
)
_SECONDARY_QSS = (
    "QPushButton { background:transparent; color:#8888a8;"
    " border:1px solid rgba(255,255,255,0.10); border-radius:6px;"
    " font-size:12px; padding:8px 18px; }"
    "QPushButton:hover { color:#d0d0e8; border-color:rgba(255,255,255,0.22); }"
)


def _elide(path: Path, max_len: int = 64) -> str:
    s = str(path)
    return s if len(s) <= max_len else "…" + s[-(max_len - 1):]


def _fmt_bytes(n: int) -> str:
    for unit in ("B", "KB", "MB", "GB"):
        if n < 1024 or unit == "GB":
            return f"{n:.0f} {unit}" if unit in ("B", "KB") else f"{n:.1f} {unit}"
        n /= 1024
    return f"{n:.1f} TB"


def _plural(n: int, word: str) -> str:
    return f"{n:,} {word}{'' if n == 1 else 's'}"


def _chip(text: str, bg: str, fg: str, tip: str = "") -> QLabel:
    lbl = QLabel(text)
    lbl.setStyleSheet(
        f"QLabel {{ background:{bg}; color:{fg}; border-radius:3px;"
        f" padding:2px 9px; font-size:11px; font-weight:600; }}"
    )
    if tip:
        lbl.setToolTip(tip)
    lbl.setSizePolicy(QSizePolicy.Fixed, QSizePolicy.Fixed)   # never squash the text
    return lbl


# ── worker ────────────────────────────────────────────────────────────────────

class _CopyWorker(QThread):
    progress = Signal(int, int, int, int)      # files done/total, bytes done/total
    done     = Signal(object, object, bool)    # copied, failed, cancelled

    def __init__(self, plan: EditCopyPlan) -> None:
        super().__init__()
        self._plan = plan
        self._cancel = False

    def cancel(self) -> None:
        self._cancel = True

    def run(self) -> None:
        copied, failed, cancelled = self._plan.execute(
            progress=lambda *a: self.progress.emit(*a),
            is_cancelled=lambda: self._cancel,
        )
        self.done.emit(copied, failed, cancelled)


# ── dialog ────────────────────────────────────────────────────────────────────

class EditCopyDialog(QDialog):
    """
    Signals
    -------
    folder_changed(Path, bool)
        The destination folder or subfolder option changed — persist it.
    copied(list, list)
        Copy finished: (copied ops, failed (op, message) pairs).
    """

    folder_changed = Signal(object, bool)
    copied         = Signal(object, object)

    def __init__(
        self,
        selection:      MarkedRaws,
        working_folder: Optional[Path],
        dest_folder:    Optional[Path],
        use_subfolder:  bool = True,
        parent=None,
    ) -> None:
        super().__init__(parent)
        self.setWindowTitle("Copy Marked RAWs for Editing")
        self.setMinimumSize(640, 560)
        self.setModal(True)
        self.setStyleSheet(
            f"QDialog {{ background:{_BG_DIALOG}; color:{_TEXT_PRI}; }}"
            f"QLabel {{ background:transparent; color:{_TEXT_PRI}; }}"
        )

        self._sel = selection
        self._working = working_folder
        self._base: Optional[Path] = dest_folder
        self._plan: Optional[EditCopyPlan] = None
        self._worker: Optional[_CopyWorker] = None
        self._finished = False

        self._build_ui(use_subfolder)
        self._rebuild()

    # ------------------------------------------------------------------ #
    # Construction                                                         #
    # ------------------------------------------------------------------ #

    def _build_ui(self, use_subfolder: bool) -> None:
        root = QVBoxLayout(self)
        root.setContentsMargins(18, 16, 18, 16)
        root.setSpacing(12)

        title = QLabel("Copy marked RAWs for editing")
        title.setStyleSheet("font-size:16px; font-weight:700; color:#f0f0f0;")
        sub = QLabel("Copies the RAW file of every marked shot into one folder, ready to "
                     "open in your editor. JPGs are left out and the originals stay put.")
        sub.setWordWrap(True)
        sub.setStyleSheet(f"font-size:12px; color:{_TEXT_SEC};")
        root.addWidget(title)
        root.addWidget(sub)

        # ── paths card ───────────────────────────────────────────────── #
        card = QWidget()
        card.setObjectName("editCard")
        card.setStyleSheet(f"#editCard {{ background:{_BG_CARD}; border:1px solid {_BORDER};"
                           " border-radius:6px; }")
        cl = QVBoxLayout(card)
        cl.setContentsMargins(14, 12, 14, 12)
        cl.setSpacing(8)

        cl.addLayout(self._path_row("📂", "FROM", _elide(self._working) if self._working else "—"))
        rule = QFrame()
        rule.setFixedHeight(1)
        rule.setStyleSheet(f"background:{_BORDER}; border:none;")
        cl.addWidget(rule)

        to_row = self._path_row("📁", "TO", "")
        self._dest_lbl = to_row.itemAt(2).widget()
        self._btn_change = QPushButton("change")
        self._btn_change.setFlat(True)
        self._btn_change.setCursor(Qt.PointingHandCursor)
        self._btn_change.setStyleSheet(
            "QPushButton { color:rgba(255,109,0,0.60); font-size:11px;"
            " background:transparent; border:none; padding:0; }"
            "QPushButton:hover { color:#ff6d00; }"
        )
        self._btn_change.clicked.connect(self._pick_folder)
        to_row.addWidget(self._btn_change)
        cl.addLayout(to_row)

        shoot = self._working.name if self._working else "shoot"
        self._chk_sub = QCheckBox(f"Put them in a subfolder named “{shoot}”")
        self._chk_sub.setStyleSheet(_CHECK_QSS)
        self._chk_sub.setChecked(use_subfolder and self._working is not None)
        self._chk_sub.setEnabled(self._working is not None)
        self._chk_sub.toggled.connect(self._on_option_changed)
        cl.addWidget(self._chk_sub)
        root.addWidget(card)

        # ── chips: what will be copied / what was left out ───────────── #
        self._chips = QHBoxLayout()
        self._notes = QHBoxLayout()
        for row in (self._chips, self._notes):
            row.setSpacing(6)
            row.setContentsMargins(0, 0, 0, 0)
            root.addLayout(row)

        # ── file list ────────────────────────────────────────────────── #
        self._list = QListWidget()
        self._list.setSelectionMode(QListWidget.NoSelection)
        self._list.setFocusPolicy(Qt.NoFocus)
        self._list.setUniformItemSizes(True)
        self._list.setStyleSheet(
            "QListWidget { background:#0f0f1c; border:1px solid rgba(255,255,255,0.05);"
            " border-radius:6px; padding:4px; font-size:12px; }"
            "QListWidget::item { padding:3px 6px; }"
            "QScrollBar:vertical { background:#0a0a14; width:6px; border:none; }"
            "QScrollBar::handle:vertical { background:#2a2a44; border-radius:3px; }"
            "QScrollBar::add-line:vertical, QScrollBar::sub-line:vertical { height:0; }"
        )
        root.addWidget(self._list, 1)

        # ── progress ─────────────────────────────────────────────────── #
        self._progress = QProgressBar()
        self._progress.setRange(0, 1000)
        self._progress.setTextVisible(False)
        self._progress.setFixedHeight(6)
        self._progress.setStyleSheet(
            "QProgressBar { background:#16162a; border:none; border-radius:3px; }"
            "QProgressBar::chunk { background:#ff6d00; border-radius:3px; }"
        )
        self._progress_lbl = QLabel()
        self._progress_lbl.setStyleSheet(f"font-size:11px; color:{_TEXT_SEC};")
        self._progress.hide()
        self._progress_lbl.hide()
        root.addWidget(self._progress)
        root.addWidget(self._progress_lbl)

        # ── footer ───────────────────────────────────────────────────── #
        foot = QHBoxLayout()
        foot.addStretch()
        self._btn_cancel = QPushButton("Cancel")
        self._btn_cancel.setStyleSheet(_SECONDARY_QSS)
        self._btn_cancel.clicked.connect(self.reject)
        self._btn_go = QPushButton("Copy")
        self._btn_go.setStyleSheet(_PRIMARY_QSS)
        self._btn_go.setDefault(True)
        self._btn_go.clicked.connect(self._on_go)
        foot.addWidget(self._btn_cancel)
        foot.addWidget(self._btn_go)
        root.addLayout(foot)

    @staticmethod
    def _path_row(emoji: str, label: str, text: str) -> QHBoxLayout:
        row = QHBoxLayout()
        row.setSpacing(10)
        icon = QLabel(emoji)
        icon.setFixedWidth(22)
        icon.setStyleSheet("font-size:15px;")
        kind = QLabel(label)
        kind.setStyleSheet(f"font-size:10px; font-weight:700; color:{_TEXT_DIM};"
                           " letter-spacing:1px; min-width:44px;")
        path = QLabel(text)
        path.setStyleSheet(f"font-size:12px; color:{_TEXT_SEC};")
        path.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Fixed)
        row.addWidget(icon)
        row.addWidget(kind)
        row.addWidget(path, 1)
        return row

    # ------------------------------------------------------------------ #
    # Plan                                                                 #
    # ------------------------------------------------------------------ #

    def dest_dir(self) -> Optional[Path]:
        if self._base is None:
            return None
        if self._chk_sub.isChecked() and self._working is not None:
            return self._base / self._working.name
        return self._base

    def _rebuild(self) -> None:
        dest = self.dest_dir()
        if dest is None:
            self._dest_lbl.setText("click “change” to choose a folder…")
            self._dest_lbl.setStyleSheet("font-size:12px; color:#4a4a68; font-style:italic;")
            self._plan = None
        else:
            self._dest_lbl.setText(_elide(dest))
            self._dest_lbl.setToolTip(str(dest))
            self._dest_lbl.setStyleSheet(f"font-size:12px; color:{_TEXT_PRI};")
            self._plan = EditCopyPlan(self._sel.raws, dest)
        self._refresh_chips()
        self._refresh_list()
        self._refresh_button()

    def _refresh_chips(self) -> None:
        for row in (self._chips, self._notes):
            while row.count():
                item = row.takeAt(0)
                if item.widget():
                    item.widget().deleteLater()
        sel, plan = self._sel, self._plan
        add = self._chips.addWidget
        note = self._notes.addWidget

        if plan is not None:
            n = len(plan.to_copy)
            add(_chip(f"{_plural(n, 'RAW file')} · {_fmt_bytes(plan.total_bytes)}",
                      "#0d1a30", "#5a8acc"))
            if plan.existing:
                add(_chip(f"{len(plan.existing)} already in folder", "#161620", "#7878a0",
                          "Identical copies from an earlier run — skipped"))
            if plan.renamed_count:
                add(_chip(f"{plan.renamed_count} renamed", "#2a2410", "#c8a040",
                          "Another file already uses the name, so these get a _1, _2… "
                          "suffix. Nothing is ever overwritten."))
            if plan.cropped_count:
                add(_chip(f"✂ {plan.cropped_count} cropped", "rgba(255,109,0,0.12)", "#ff6d00",
                          "The crop is written to an .xmp sidecar next to the RAW, "
                          "so your editor opens it cropped"))
        else:
            add(_chip(f"{_plural(len(sel.raws), 'RAW file')}", "#0d1a30", "#5a8acc"))

        if sel.matched_by_name:
            note(_chip(f"{sel.matched_by_name} matched by name", "#161620", "#7878a0",
                      "Marked JPGs whose RAW has the same file name but isn't paired yet"))
        if sel.jpg_only:
            names = ", ".join(r.filename for r in sel.jpg_only[:12])
            more = f" and {len(sel.jpg_only) - 12} more" if len(sel.jpg_only) > 12 else ""
            note(_chip(f"⚠ {len(sel.jpg_only)} JPG-only — no RAW", "#2a2410", "#c8a040",
                      f"Marked, but there is no RAW for these — not copied:\n{names}{more}"))
        if sel.pruned:
            note(_chip(f"{sel.pruned} pruned — skipped", "#2a0d0d", "#cc4848",
                      "Marked shots that are also pruned are left out"))
        self._chips.addStretch()
        self._notes.addStretch()

    def _refresh_list(self) -> None:
        self._list.clear()
        if not self._sel.raws:
            item = QListWidgetItem("No marked RAW files. Mark shots with M in the grid or viewer first.")
            item.setForeground(QColor("#6a6a8a"))
            self._list.addItem(item)
            return
        if self._plan is None:
            for r in sorted(self._sel.raws, key=lambda r: (r.shot_time, r.filename.lower())):
                self._list.addItem(QListWidgetItem(r.filename))
            return
        for op in self._plan.ops:
            text = op.record.filename
            colour = QColor(_TEXT_PRI)
            if op.exists:
                text += "   ·   already in folder"
                colour = QColor("#50506a")
            elif op.renamed:
                text += f"   →   {op.dest.name}   (name taken)"
                colour = QColor("#c8a040")
            if op.record.is_cropped and not op.exists:
                text += "   ✂"
            item = QListWidgetItem(text)
            item.setForeground(colour)
            item.setToolTip(str(op.record.path))
            self._list.addItem(item)

    def _refresh_button(self) -> None:
        n = len(self._plan.to_copy) if self._plan else 0
        self._btn_go.setEnabled(n > 0)
        if self._plan is None:
            self._btn_go.setText("Copy")
        elif n == 0 and self._plan.existing:
            self._btn_go.setText("All files already copied")
        else:
            self._btn_go.setText(f"Copy {_plural(n, 'RAW File')}")

    # ------------------------------------------------------------------ #
    # Options                                                              #
    # ------------------------------------------------------------------ #

    def _pick_folder(self) -> None:
        folder = QFileDialog.getExistingDirectory(
            self, "Folder to Copy RAWs Into",
            str(self._base or QDir.homePath()),
            QFileDialog.ShowDirsOnly | QFileDialog.DontResolveSymlinks,
        )
        if folder:
            self._base = Path(folder)
            self._on_option_changed()

    def _on_option_changed(self, *_args) -> None:
        if self._base is not None:
            self.folder_changed.emit(self._base, self._chk_sub.isChecked())
        self._rebuild()

    # ------------------------------------------------------------------ #
    # Copying                                                              #
    # ------------------------------------------------------------------ #

    def _on_go(self) -> None:
        if self._finished:
            self._open_folder()
            return
        if self._plan is None or not self._plan.to_copy:
            return
        # Re-plan right before copying: the folder may have changed while the
        # dialog sat open.
        self._plan = EditCopyPlan(self._sel.raws, self.dest_dir())
        if not self._plan.to_copy:
            self._rebuild()
            return
        for w in (self._btn_go, self._btn_change, self._chk_sub):
            w.setEnabled(False)
        self._btn_cancel.setText("Stop")
        self._progress.setValue(0)
        self._progress.show()
        self._progress_lbl.setText("Starting…")
        self._progress_lbl.show()

        self._worker = _CopyWorker(self._plan)
        self._worker.progress.connect(self._on_progress)
        self._worker.done.connect(self._on_done)
        self._worker.start()

    def _on_progress(self, n: int, total: int, b: int, btotal: int) -> None:
        self._progress.setValue(int(1000 * b / btotal) if btotal else 1000)
        self._progress_lbl.setText(
            f"{n} / {total} files   ·   {_fmt_bytes(b)} of {_fmt_bytes(btotal)}")

    def _on_done(self, copied: List, failed: List, cancelled: bool) -> None:
        self._worker.wait()
        self._worker = None
        self._finished = True
        self._progress.hide()
        self.copied.emit(copied, failed)

        self._list.clear()
        head = f"✓  {_plural(len(copied), 'RAW file')} copied"
        if cancelled:
            head += "  —  stopped early"
        if failed:
            head += f"  —  {len(failed)} failed"
        item = QListWidgetItem(head)
        item.setForeground(QColor("#b89030" if (failed or cancelled) else "#4a9a60"))
        f = item.font()
        f.setBold(True)
        item.setFont(f)
        self._list.addItem(item)
        for op, err in failed[:50]:
            e = QListWidgetItem(f"✗  {op.record.filename}  —  {err}")
            e.setForeground(QColor("#c05050"))
            self._list.addItem(e)
        self._progress_lbl.setText(_elide(self.dest_dir(), 90))
        self._progress_lbl.setToolTip(str(self.dest_dir()))

        self._btn_go.setText("Open Folder")
        self._btn_go.setEnabled(True)
        self._btn_cancel.setText("Close")
        self._btn_cancel.setEnabled(True)

    def _open_folder(self) -> None:
        dest = self.dest_dir()
        if dest is not None:
            try:
                subprocess.Popen(["xdg-open", str(dest)])
            except Exception:  # noqa: BLE001
                pass

    def reject(self) -> None:
        # Stop first; the dialog closes once the worker reports back
        if self._worker is not None:
            self._worker.cancel()
            self._btn_cancel.setEnabled(False)
            self._progress_lbl.setText("Stopping after the current file…")
            return
        super().reject()
