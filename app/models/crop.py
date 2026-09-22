"""
Crop rectangle model.

A Crop is stored in **normalised image coordinates** (0.0 – 1.0) relative to
the *orientation-corrected* image — i.e. the image as the viewer displays it
after EXIF rotation has been applied.

Normalised coordinates are what make a single crop valid for both halves of a
RAW+JPG pair: the two files almost never have identical pixel dimensions (most
bodies emit a RAW a few pixels wider than the camera-rendered JPEG), but they
frame the same scene.  A fractional rect maps cleanly onto both.

AspectRatio
    A named w:h constraint.  `PRESETS` holds the common photographic ratios;
    `AspectRatio.custom(w, h)` builds an arbitrary one.

Crop
    Immutable normalised rect + the aspect label it was made with, so the
    viewer can restore the correct ratio button when you come back to a photo.
"""
from __future__ import annotations

from dataclasses import dataclass
from math import isclose
from typing import Dict, List, Optional, Tuple

# Smallest crop we allow, as a fraction of the image edge.  Stops a stray
# double-click from collapsing the rect to zero area.
MIN_EDGE = 0.02


# ── aspect ratios ──────────────────────────────────────────────────────────── #

@dataclass(frozen=True)
class AspectRatio:
    """A named width:height constraint.  `value` is None for freeform."""

    label: str
    w:     Optional[float] = None
    h:     Optional[float] = None

    @property
    def is_free(self) -> bool:
        return self.w is None or self.h is None

    @property
    def value(self) -> Optional[float]:
        """width / height, or None when freeform."""
        if self.is_free:
            return None
        return self.w / self.h

    def flipped(self) -> "AspectRatio":
        """Swap orientation — 3:2 landscape becomes 2:3 portrait."""
        if self.is_free:
            return self
        return AspectRatio(f"{self.h:g}:{self.w:g}", self.h, self.w)

    @staticmethod
    def custom(w: float, h: float) -> "AspectRatio":
        return AspectRatio(f"{w:g}:{h:g}", float(w), float(h))


FREEFORM = AspectRatio("Freeform")
ORIGINAL = AspectRatio("Original")     # resolved against the live image size

# Ordered as they appear in the crop bar.
PRESETS: List[AspectRatio] = [
    FREEFORM,
    ORIGINAL,
    AspectRatio("1:1",  1, 1),
    AspectRatio("3:2",  3, 2),
    AspectRatio("4:3",  4, 3),
    AspectRatio("5:4",  5, 4),
    AspectRatio("7:5",  7, 5),
    AspectRatio("16:9", 16, 9),
    AspectRatio("16:10", 16, 10),
    AspectRatio("65:24", 65, 24),      # XPan / panoramic
]

_BY_LABEL: Dict[str, AspectRatio] = {a.label: a for a in PRESETS}


def ratio_from_label(label: str) -> Optional[AspectRatio]:
    """
    Resolve a stored label back to an AspectRatio.

    Handles the presets, their flipped variants, and any "w:h" custom string
    round-tripped through the sidecar.
    """
    if not label:
        return None
    if label in _BY_LABEL:
        return _BY_LABEL[label]
    for preset in PRESETS:
        if not preset.is_free and preset.flipped().label == label:
            return preset.flipped()
    if ":" in label:
        try:
            w_s, h_s = label.split(":", 1)
            w, h = float(w_s), float(h_s)
            if w > 0 and h > 0:
                return AspectRatio.custom(w, h)
        except ValueError:
            pass
    return None


# ── crop rect ──────────────────────────────────────────────────────────────── #

@dataclass(frozen=True)
class Crop:
    """
    Normalised crop rect on the orientation-corrected image.

    All four values are fractions in [0, 1] with left < right and top < bottom.
    """

    left:   float
    top:    float
    right:  float
    bottom: float
    aspect_label: Optional[str] = None   # ratio in force when the crop was made

    # ── construction ──────────────────────────────────────────────────── #

    def __post_init__(self) -> None:
        # Clamp and normalise in one pass.  frozen=True means object.__setattr__.
        l, t = max(0.0, min(1.0, self.left)), max(0.0, min(1.0, self.top))
        r, b = max(0.0, min(1.0, self.right)), max(0.0, min(1.0, self.bottom))
        if r < l:
            l, r = r, l
        if b < t:
            t, b = b, t
        object.__setattr__(self, "left", l)
        object.__setattr__(self, "top", t)
        object.__setattr__(self, "right", max(r, l + MIN_EDGE) if r - l < MIN_EDGE else r)
        object.__setattr__(self, "bottom", max(b, t + MIN_EDGE) if b - t < MIN_EDGE else b)

    @staticmethod
    def full(aspect_label: Optional[str] = None) -> "Crop":
        return Crop(0.0, 0.0, 1.0, 1.0, aspect_label)

    # ── geometry ──────────────────────────────────────────────────────── #

    @property
    def width(self) -> float:
        return self.right - self.left

    @property
    def height(self) -> float:
        return self.bottom - self.top

    @property
    def is_full(self) -> bool:
        """True when the crop covers the whole frame (nothing to apply)."""
        return (
            isclose(self.left, 0.0, abs_tol=1e-4)
            and isclose(self.top, 0.0, abs_tol=1e-4)
            and isclose(self.right, 1.0, abs_tol=1e-4)
            and isclose(self.bottom, 1.0, abs_tol=1e-4)
        )

    def to_pixels(self, img_w: int, img_h: int) -> Tuple[int, int, int, int]:
        """
        Convert to an integer pixel box (left, top, right, bottom) for *img_w*
        × *img_h*, guaranteed to be at least 1×1 and inside the image.
        """
        # Round the origin and the *size* independently rather than rounding
        # both edges: rounding each edge lets two half-pixel errors stack and
        # knock an exact 16:9 crop off by a pixel.
        l = max(0, min(int(round(self.left * img_w)), img_w - 1))
        t = max(0, min(int(round(self.top * img_h)), img_h - 1))
        w = max(1, int(round(self.width * img_w)))
        h = max(1, int(round(self.height * img_h)))
        r = min(l + w, img_w)
        b = min(t + h, img_h)
        # Clipping at the far edge would silently change the ratio; pull the
        # origin back instead so the requested size survives.
        l = max(0, r - w)
        t = max(0, b - h)
        return l, t, r, b

    def pixel_size(self, img_w: int, img_h: int) -> Tuple[int, int]:
        l, t, r, b = self.to_pixels(img_w, img_h)
        return r - l, b - t

    # ── serialisation ─────────────────────────────────────────────────── #

    def to_dict(self) -> dict:
        d = {
            "left":   round(self.left, 6),
            "top":    round(self.top, 6),
            "right":  round(self.right, 6),
            "bottom": round(self.bottom, 6),
        }
        if self.aspect_label:
            d["aspect"] = self.aspect_label
        return d

    @staticmethod
    def from_dict(d: dict) -> Optional["Crop"]:
        """Parse a sidecar entry.  Returns None if the entry is malformed."""
        try:
            return Crop(
                float(d["left"]),
                float(d["top"]),
                float(d["right"]),
                float(d["bottom"]),
                d.get("aspect") or None,
            )
        except (KeyError, TypeError, ValueError):
            return None
