"""
CropBar — the aspect-ratio strip shown beneath the viewer toolbar while the
crop tool is active.

Layout
------
[Freeform][Original][1:1][3:2][4:3]… [⇅] [w]:[h][Set]
                    … 4000 × 2667 px  [Reset][Cancel][Apply]

The strip has to survive a narrow viewer window with the EXIF panel open, so
it carries no decorative labels and every control is sized to its content.

Signals
-------
aspect_selected(AspectRatio)   a ratio button (or Custom) was chosen
reset_requested()              Reset clicked
cancel_requested()             Cancel clicked — discard this session's edits
apply_requested()              Apply clicked — commit the crop
"""
from __future__ import annotations

from typing import Dict, Optional

from PySide6.QtCore import Qt, Signal
from PySide6.QtWidgets import (
    QHBoxLayout,
    QLabel,
    QPushButton,
    QSizePolicy,
    QSpinBox,
    QWidget,
)

from app.models.crop import FREEFORM, PRESETS, AspectRatio

_ORANGE = "#ff6d00"

_RATIO_QSS = (
    "QPushButton{background:rgba(255,109,0,0.07);color:#8888a8;"
    "border:1px solid rgba(255,109,0,0.16);border-radius:4px;"
    "font-size:11px;padding:2px 8px;}"
    "QPushButton:hover{background:rgba(255,109,0,0.18);color:#f0f0f0;"
    "border-color:rgba(255,109,0,0.45);}"
    "QPushButton:checked{background:rgba(255,109,0,0.24);color:#ff6d00;"
    "border-color:rgba(255,109,0,0.60);font-weight:bold;}"
)

_SPIN_QSS = (
    "QSpinBox{background:#12121f;color:#d0d0e8;"
    "border:1px solid rgba(255,109,0,0.20);border-radius:3px;"
    "font-size:11px;padding:1px 4px;}"
    "QSpinBox:focus{border-color:rgba(255,109,0,0.55);}"
)


def _action_btn(text: str, accent: bool = False) -> QPushButton:
    b = QPushButton(text)
    b.setFixedHeight(24)
    b.setSizePolicy(QSizePolicy.Fixed, QSizePolicy.Fixed)
    if accent:
        b.setStyleSheet(
            "QPushButton{background:rgba(255,109,0,0.85);color:#140a00;"
            "border:1px solid #ff6d00;border-radius:4px;"
            "font-size:11px;font-weight:bold;padding:2px 12px;}"
            "QPushButton:hover{background:#ff8524;}"
        )
    else:
        b.setStyleSheet(
            "QPushButton{background:rgba(255,109,0,0.07);color:#8888a8;"
            "border:1px solid rgba(255,109,0,0.18);border-radius:4px;"
            "font-size:11px;padding:2px 10px;}"
            "QPushButton:hover{background:rgba(255,109,0,0.18);color:#f0f0f0;"
            "border-color:rgba(255,109,0,0.45);}"
        )
    return b


class CropBar(QWidget):
    """Ratio presets, orientation flip, custom ratio, and the commit buttons."""

    aspect_selected:  Signal = Signal(object)   # AspectRatio
    reset_requested:  Signal = Signal()
    cancel_requested: Signal = Signal()
    apply_requested:  Signal = Signal()

    def __init__(self, parent=None) -> None:
        super().__init__(parent)
        self.setFixedHeight(34)
        # Scoped to this widget by object name — an unscoped rule would be
        # inherited by every child and draw the bar's border around each
        # label inside it.
        self.setObjectName("cropBar")
        self.setStyleSheet(
            "#cropBar {"
            "  background:#0b0b16;"
            "  border-bottom:1px solid rgba(255,109,0,0.16);"
            "}"
        )

        self._buttons: Dict[str, QPushButton] = {}
        self._current: AspectRatio = FREEFORM
        # Presets are stored landscape-first; this tracks whether the user has
        # flipped them to portrait so the button labels stay honest.
        self._portrait: bool = False

        lay = QHBoxLayout(self)
        lay.setContentsMargins(8, 0, 8, 0)
        lay.setSpacing(3)

        for preset in PRESETS:
            b = QPushButton(preset.label)
            b.setCheckable(True)
            b.setFixedHeight(24)
            b.setStyleSheet(_RATIO_QSS)
            b.setSizePolicy(QSizePolicy.Fixed, QSizePolicy.Fixed)
            b.clicked.connect(
                lambda _checked=False, p=preset: self._on_preset_clicked(p)
            )
            self._buttons[preset.label] = b
            lay.addWidget(b)

        self._btn_flip = QPushButton("⇅")
        self._btn_flip.setFixedSize(26, 24)
        self._btn_flip.setToolTip("Swap orientation (portrait ↔ landscape)")
        self._btn_flip.setStyleSheet(_RATIO_QSS)
        self._btn_flip.clicked.connect(self._on_flip)
        lay.addSpacing(4)
        lay.addWidget(self._btn_flip)

        # ── custom ratio ────────────────────────────────────────────────
        # No "Custom" caption: the two number boxes next to the presets read
        # unambiguously, and the label was the first thing to clip on a
        # narrow window.
        lay.addSpacing(6)

        self._spin_w = QSpinBox()
        self._spin_h = QSpinBox()
        for spin, initial in ((self._spin_w, 3), (self._spin_h, 2)):
            spin.setRange(1, 999)
            spin.setValue(initial)
            spin.setFixedSize(42, 22)
            spin.setAlignment(Qt.AlignCenter)
            spin.setButtonSymbols(QSpinBox.NoButtons)
            spin.setStyleSheet(_SPIN_QSS)
            spin.setToolTip("Custom aspect ratio — press Enter or click Set")
            # returnPressed, not editingFinished: the latter also fires on
            # focus-out, so clicking a preset button would apply the custom
            # ratio on the way there and reshape the rect twice.
            spin.lineEdit().returnPressed.connect(self._on_custom)
        colon = QLabel(":")
        colon.setStyleSheet("color:#44445a;font-size:11px;")
        colon.setSizePolicy(QSizePolicy.Fixed, QSizePolicy.Fixed)

        lay.addWidget(self._spin_w)
        lay.addWidget(colon)
        lay.addWidget(self._spin_h)

        self._btn_custom = _action_btn("Set")
        self._btn_custom.setToolTip("Apply the custom ratio")
        self._btn_custom.clicked.connect(self._on_custom)
        lay.addWidget(self._btn_custom)

        lay.addStretch(1)

        # ── live size readout ───────────────────────────────────────────
        self._lbl_size = QLabel("—")
        self._lbl_size.setStyleSheet(
            "color:#7878a0;font-size:11px;font-family:monospace;"
        )
        self._lbl_size.setAlignment(Qt.AlignRight | Qt.AlignVCenter)
        self._lbl_size.setSizePolicy(QSizePolicy.Fixed, QSizePolicy.Fixed)
        self._lbl_size.setMinimumWidth(150)
        lay.addWidget(self._lbl_size)
        lay.addSpacing(8)

        self._btn_reset  = _action_btn("Reset")
        self._btn_cancel = _action_btn("Cancel")
        self._btn_apply  = _action_btn("Apply", accent=True)
        self._btn_reset.clicked.connect(self.reset_requested.emit)
        self._btn_cancel.clicked.connect(self.cancel_requested.emit)
        self._btn_apply.clicked.connect(self.apply_requested.emit)
        lay.addWidget(self._btn_reset)
        lay.addWidget(self._btn_cancel)
        lay.addWidget(self._btn_apply)

        self._sync_checked(FREEFORM)

    # ------------------------------------------------------------------ #
    # Public                                                               #
    # ------------------------------------------------------------------ #

    def current_aspect(self) -> AspectRatio:
        return self._current

    def set_size_readout(self, w: int, h: int, src_w: int, src_h: int) -> None:
        """Show the crop's output pixel size and what fraction of the source it keeps."""
        if w <= 0 or h <= 0 or src_w <= 0 or src_h <= 0:
            self._lbl_size.setText("—")
            return
        pct = 100.0 * (w * h) / (src_w * src_h)
        self._lbl_size.setText(f"{w} × {h} px  •  {pct:.0f}%")

    def select_aspect(self, ratio: Optional[AspectRatio]) -> None:
        """
        Reflect *ratio* in the UI without emitting aspect_selected.

        Used when opening the crop tool on a photo that already has a crop —
        the ratio it was made with gets its button lit again.
        """
        ratio = ratio or FREEFORM
        self._current = ratio
        if not ratio.is_free and ratio.w is not None and ratio.h is not None:
            self._portrait = ratio.h > ratio.w
            self._spin_w.setValue(int(round(ratio.w)))
            self._spin_h.setValue(int(round(ratio.h)))
        self._sync_checked(ratio)

    # ------------------------------------------------------------------ #
    # Internals                                                            #
    # ------------------------------------------------------------------ #

    def _oriented(self, preset: AspectRatio) -> AspectRatio:
        """Apply the current portrait/landscape state to a stored preset."""
        if preset.is_free or not self._portrait:
            return preset
        return preset.flipped()

    def _sync_checked(self, active: AspectRatio) -> None:
        """Light the matching button and relabel presets for the orientation."""
        for preset in PRESETS:
            btn = self._buttons[preset.label]
            shown = self._oriented(preset)
            btn.setText(shown.label)
            btn.setChecked(shown.label == active.label)

        # A custom ratio matches no preset; make that visible.
        any_checked = any(b.isChecked() for b in self._buttons.values())
        self._btn_custom.setEnabled(True)
        self._btn_flip.setEnabled(not active.is_free)
        if not any_checked and not active.is_free:
            self._spin_w.setValue(int(round(active.w)))
            self._spin_h.setValue(int(round(active.h)))

    def _select(self, ratio: AspectRatio) -> None:
        self._current = ratio
        self._sync_checked(ratio)
        self.aspect_selected.emit(ratio)

    def _on_preset_clicked(self, preset: AspectRatio) -> None:
        self._select(self._oriented(preset))

    def _on_flip(self) -> None:
        if self._current.is_free:
            return
        self._portrait = not self._portrait
        self._select(self._current.flipped())

    def _on_custom(self) -> None:
        self._portrait = self._spin_h.value() > self._spin_w.value()
        self._select(AspectRatio.custom(self._spin_w.value(), self._spin_h.value()))
