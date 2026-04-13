"""
FilterBar — filter toggles, sort controls, and date-range picker.

Emits filter_changed(FilterState) on every change.
"""
from __future__ import annotations

from datetime import date as _pydate
from typing import List, Optional

from PySide6.QtCore import QDate, Qt, Signal
from PySide6.QtWidgets import (
    QAbstractSpinBox,
    QDateEdit,
    QFrame,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QSlider,
    QWidget,
)

from app.ui.proxy import FilterState

# ── palette ──────────────────────────────────────────────────────────────── #
_ACCENT = "#ff6d00"

# Sort buttons: (display label, sort_key)
_SORT_OPTIONS: list[tuple[str, str]] = [
    ("Date", "date"),
    ("Name", "name"),
    ("Size", "size"),
]

_DATE_QSS = (
    "QDateEdit {"
    "  background: rgba(255,109,0,0.08);"
    "  color: #8888a8;"
    "  border: 1px solid rgba(255,109,0,0.20);"
    "  border-radius: 4px;"
    "  padding: 1px 6px;"
    "  font-size: 11px;"
    "}"
    "QDateEdit:focus {"
    "  border-color: rgba(255,109,0,0.50);"
    "  color: #f0f0f0;"
    "}"
    "QCalendarWidget QWidget { background: #13131f; color: #f0f0f0; }"
    "QCalendarWidget QAbstractItemView:enabled {"
    "  background: #13131f; color: #c8c8d8;"
    "  selection-background-color: rgba(255,109,0,0.30);"
    "  selection-color: #ff6d00;"
    "}"
    "QCalendarWidget QToolButton {"
    "  background: transparent; color: #8888a8; border: none;"
    "}"
    "QCalendarWidget QToolButton:hover {"
    "  background: rgba(255,109,0,0.15); color: #f0f0f0;"
    "}"
    "QCalendarWidget #qt_calendar_navigationbar {"
    "  background: #0e0e1a; border-bottom: 1px solid rgba(255,109,0,0.15);"
    "}"
)


# ── reusable helpers ──────────────────────────────────────────────────────── #

def _vline() -> QFrame:
    sep = QFrame()
    sep.setFrameShape(QFrame.VLine)
    sep.setStyleSheet("QFrame { color: rgba(255,109,0,0.12); margin: 4px 2px; }")
    return sep


def _qdate_to_py(qd: QDate) -> Optional[_pydate]:
    return _pydate(qd.year(), qd.month(), qd.day()) if qd.isValid() else None


def _py_to_qdate(d: Optional[_pydate]) -> QDate:
    return QDate(d.year, d.month, d.day) if d else QDate()


# ── widget classes ────────────────────────────────────────────────────────── #

class _ToggleButton(QPushButton):
    """Pill-shaped checkable button; active = coloured border + rgba tint."""

    def __init__(self, label: str, accent: str, *, checked: bool = True) -> None:
        super().__init__(label)
        self.setCheckable(True)
        r, g, b = int(accent[1:3], 16), int(accent[3:5], 16), int(accent[5:7], 16)
        tint       = f"rgba({r},{g},{b},35)"
        tint_hover = f"rgba({r},{g},{b},60)"
        # Pre-compute both states once — avoids string work on every toggle
        self._ss_on = (
            f"QPushButton {{"
            f"  background: {tint}; color: #ddd;"
            f"  border: 1px solid {accent};"
            f"  border-radius: 4px; padding: 2px 11px;"
            f"  font-size: 11px; font-weight: 600;"
            f"}}"
            f"QPushButton:hover {{ background: {tint_hover}; }}"
        )
        self._ss_off = (
            "QPushButton {"
            "  background: transparent; color: #44445a;"
            "  border: 1px solid #1e1e2e; border-radius: 4px;"
            "  padding: 2px 11px; font-size: 11px; font-weight: 600;"
            "}"
            "QPushButton:hover {"
            "  background: rgba(255,109,0,0.06); color: #7878a0;"
            "  border-color: rgba(255,109,0,0.20);"
            "}"
        )
        self.setChecked(checked)
        self._refresh()
        self.toggled.connect(self._refresh)

    def _refresh(self) -> None:
        self.setStyleSheet(self._ss_on if self.isChecked() else self._ss_off)


class _SortButton(QPushButton):
    """
    Exclusive sort-key button.

    • Clicking an *inactive* button activates it (direction resets to asc).
    • Clicking the *active* button toggles direction (↑ / ↓).
    Other sort buttons must be deactivated by the parent (FilterBar).
    """

    def __init__(self, label: str, key: str) -> None:
        super().__init__(label)
        self._label   = label
        self._key     = key
        self._active  = False
        self._asc     = True
        self._refresh()

    # ------------------------------------------------------------------ #
    # Properties                                                           #
    # ------------------------------------------------------------------ #

    @property
    def sort_key(self) -> str:
        return self._key

    @property
    def is_active(self) -> bool:
        return self._active

    @property
    def ascending(self) -> bool:
        return self._asc

    # ------------------------------------------------------------------ #
    # Mutation                                                             #
    # ------------------------------------------------------------------ #

    def set_active(self, active: bool, ascending: bool = True) -> None:
        self._active = active
        self._asc    = ascending
        self._refresh()

    def toggle_direction(self) -> None:
        self._asc = not self._asc
        self._refresh()

    # ------------------------------------------------------------------ #
    # Style                                                                #
    # ------------------------------------------------------------------ #

    # Class-level pre-computed stylesheets (shared across all instances)
    _SS_ACTIVE = (
        "QPushButton {"
        "  background: rgba(255,109,0,0.12); color: #ff6d00;"
        "  border: 1px solid rgba(255,109,0,0.40);"
        "  border-radius: 4px; padding: 2px 10px;"
        "  font-size: 11px; font-weight: 600;"
        "}"
        "QPushButton:hover { background: rgba(255,109,0,0.20); }"
    )
    _SS_INACTIVE = (
        "QPushButton {"
        "  background: transparent; color: #44445a;"
        "  border: 1px solid transparent;"
        "  border-radius: 4px; padding: 2px 10px;"
        "  font-size: 11px; font-weight: 600;"
        "}"
        "QPushButton:hover {"
        "  background: rgba(255,109,0,0.06); color: #7878a0;"
        "  border-color: rgba(255,109,0,0.15);"
        "}"
    )

    def _refresh(self) -> None:
        text = self._label + (" ↑" if self._asc else " ↓") if self._active else self._label
        self.setText(text)
        self.setStyleSheet(self._SS_ACTIVE if self._active else self._SS_INACTIVE)


# ── main widget ───────────────────────────────────────────────────────────── #

class FilterBar(QWidget):
    """
    Horizontal strip:
      [Show Pruned]  ←→  [Date↑][Name][Size] | [📅][from][–][to][✕] | [All]

    Signals
    -------
    filter_changed(FilterState)
    """

    filter_changed: Signal = Signal(object)
    zoom_changed:   Signal = Signal(int)

    def __init__(self, parent=None) -> None:
        super().__init__(parent)
        self.setStyleSheet(
            "FilterBar { background: #14142a;"
            " border-bottom: 1px solid rgba(255,109,0,0.15); }"
        )

        lay = QHBoxLayout(self)
        lay.setContentsMargins(10, 4, 10, 4)
        lay.setSpacing(4)

        # ── pruned toggle (unchecked by default — pruned hidden normally) #
        self._show_pruned = _ToggleButton("Show Pruned", "#cc3030", checked=False)
        self._show_pruned.toggled.connect(self._emit)
        lay.addWidget(self._show_pruned)

        lay.addStretch()

        # ── sort buttons ─────────────────────────────────────────────── #
        lay.addWidget(_vline())
        self._sort_btns: List[_SortButton] = []
        for label, key in _SORT_OPTIONS:
            btn = _SortButton(label, key)
            btn.clicked.connect(lambda _checked, b=btn: self._on_sort_click(b))
            lay.addWidget(btn)
            self._sort_btns.append(btn)
        # Default: Date ascending
        self._sort_btns[0].set_active(True, ascending=True)

        # ── date range ───────────────────────────────────────────────── #
        lay.addWidget(_vline())

        self._date_toggle = _ToggleButton("📅", _ACCENT, checked=False)
        self._date_toggle.setToolTip("Filter by date range")
        self._date_toggle.toggled.connect(self._on_date_toggle)
        lay.addWidget(self._date_toggle)

        today = QDate.currentDate()
        year_start = QDate(today.year(), 1, 1)

        self._date_from = self._make_date_edit(year_start)
        self._date_dash = QLabel("–")
        self._date_dash.setStyleSheet("color: #44445a; font-size: 11px;")
        self._date_to = self._make_date_edit(today)

        for w in (self._date_from, self._date_dash, self._date_to):
            lay.addWidget(w)
            w.setVisible(False)

        self._date_clear = QPushButton("✕")
        self._date_clear.setFixedSize(22, 22)
        self._date_clear.setToolTip("Clear date range")
        self._date_clear.setStyleSheet(
            "QPushButton { background: transparent; color: #44445a;"
            " border: none; font-size: 11px; }"
            "QPushButton:hover { color: #cc3030; }"
        )
        self._date_clear.clicked.connect(self._clear_date_range)
        self._date_clear.setVisible(False)
        lay.addWidget(self._date_clear)

        # ── All reset button ──────────────────────────────────────────── #
        lay.addSpacing(2)
        all_btn = QPushButton("All")
        all_btn.setFixedWidth(38)
        all_btn.setToolTip("Reset all filters")
        all_btn.setStyleSheet(
            "QPushButton { background: transparent; color: #44445a;"
            " border: 1px solid #1e1e2e; border-radius: 4px;"
            " padding: 2px 6px; font-size: 11px; }"
            "QPushButton:hover { background: rgba(255,109,0,0.08); color: #7878a0;"
            " border-color: rgba(255,109,0,0.25); }"
        )
        all_btn.clicked.connect(self._reset_all)
        lay.addWidget(all_btn)

        # ── zoom slider ───────────────────────────────────────────── #
        lay.addWidget(_vline())
        self._zoom_slider = QSlider(Qt.Horizontal)
        self._zoom_slider.setRange(120, 480)
        self._zoom_slider.setSingleStep(20)
        self._zoom_slider.setPageStep(20)
        self._zoom_slider.setValue(180)
        self._zoom_slider.setFixedWidth(90)
        self._zoom_slider.setToolTip("Thumbnail size  (Ctrl+scroll)")
        self._zoom_slider.setStyleSheet(
            "QSlider::groove:horizontal {"
            "  background: rgba(255,109,0,0.15); border-radius: 2px; height: 3px; }"
            "QSlider::handle:horizontal {"
            "  background: #ff6d00; border-radius: 4px;"
            "  width: 10px; height: 10px; margin: -4px 0; }"
            "QSlider::sub-page:horizontal {"
            "  background: rgba(255,109,0,0.50); border-radius: 2px; }"
            "QSlider:disabled { opacity: 0.3; }"
        )
        self._zoom_slider.valueChanged.connect(self.zoom_changed)
        lay.addWidget(self._zoom_slider)

    # ------------------------------------------------------------------ #
    # Public API                                                           #
    # ------------------------------------------------------------------ #

    def set_zoom(self, size: int) -> None:
        self._zoom_slider.blockSignals(True)
        self._zoom_slider.setValue(size)
        self._zoom_slider.blockSignals(False)

    def set_zoom_enabled(self, enabled: bool) -> None:
        self._zoom_slider.setEnabled(enabled)

    def current_state(self) -> FilterState:
        # sort
        sort_key, sort_asc = "date", True
        for btn in self._sort_btns:
            if btn.is_active:
                sort_key, sort_asc = btn.sort_key, btn.ascending
                break

        # date range
        date_from = date_to = None
        if self._date_toggle.isChecked():
            date_from = _qdate_to_py(self._date_from.date())
            date_to   = _qdate_to_py(self._date_to.date())

        showing_pruned = self._show_pruned.isChecked()
        return FilterState(
            show_raw      = True,
            show_jpg      = True,
            show_paired   = True,
            show_unpaired = True,
            show_pruned   = showing_pruned,
            show_unpruned = True,
            date_from     = date_from,
            date_to       = date_to,
            sort_key      = sort_key,
            sort_asc      = sort_asc,
        )

    def restore_state(
        self,
        show_pruned: bool = False,
        sort_key:  str  = "date",
        sort_asc:  bool = True,
        date_from: Optional[_pydate] = None,
        date_to:   Optional[_pydate] = None,
        **_ignored,   # absorb any extra saved keys from older sessions
    ) -> None:
        """Restore bar state without emitting intermediate signals."""
        self._show_pruned.blockSignals(True)
        self._show_pruned.setChecked(show_pruned)
        self._show_pruned.blockSignals(False)

        # sort buttons
        for btn in self._sort_btns:
            btn.set_active(btn.sort_key == sort_key,
                           sort_asc if btn.sort_key == sort_key else True)

        # date range
        date_active = date_from is not None or date_to is not None
        self._date_toggle.blockSignals(True)
        self._date_toggle.setChecked(date_active)
        self._date_toggle.blockSignals(False)
        if date_from:
            self._date_from.setDate(_py_to_qdate(date_from))
        if date_to:
            self._date_to.setDate(_py_to_qdate(date_to))
        for w in (self._date_from, self._date_dash, self._date_to, self._date_clear):
            w.setVisible(date_active)

        self._emit()

    # ------------------------------------------------------------------ #
    # Internal slots                                                       #
    # ------------------------------------------------------------------ #

    def _emit(self) -> None:
        self.filter_changed.emit(self.current_state())

    def _on_sort_click(self, clicked: _SortButton) -> None:
        if clicked.is_active:
            clicked.toggle_direction()
        else:
            clicked.set_active(True, ascending=True)
            for btn in self._sort_btns:
                if btn is not clicked:
                    btn.set_active(False)
        self._emit()

    def _on_date_toggle(self, checked: bool) -> None:
        for w in (self._date_from, self._date_dash, self._date_to, self._date_clear):
            w.setVisible(checked)
        self._emit()

    def _clear_date_range(self) -> None:
        today = QDate.currentDate()
        self._date_from.blockSignals(True)
        self._date_to.blockSignals(True)
        self._date_from.setDate(QDate(today.year(), 1, 1))
        self._date_to.setDate(today)
        self._date_from.blockSignals(False)
        self._date_to.blockSignals(False)
        self._emit()

    def _reset_all(self) -> None:
        """Reset filter toggle and date range; sort is left unchanged."""
        self._show_pruned.blockSignals(True)
        self._show_pruned.setChecked(False)
        self._show_pruned.blockSignals(False)
        self._date_toggle.blockSignals(True)
        self._date_toggle.setChecked(False)
        self._date_toggle.blockSignals(False)
        for w in (self._date_from, self._date_dash, self._date_to, self._date_clear):
            w.setVisible(False)
        self._emit()

    # ------------------------------------------------------------------ #
    # Helpers                                                              #
    # ------------------------------------------------------------------ #

    @staticmethod
    def _make_date_edit(default: QDate) -> QDateEdit:
        de = QDateEdit()
        de.setCalendarPopup(True)
        de.setButtonSymbols(QAbstractSpinBox.NoButtons)
        de.setDisplayFormat("MMM d  yyyy")
        de.setFixedWidth(100)
        de.setDate(default)
        de.setStyleSheet(_DATE_QSS)
        return de
