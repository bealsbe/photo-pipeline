"""
app/ops/crop_export.py — turning a stored Crop into something on disk.

Two very different jobs, because the two file types are not alike:

JPG
    `render_cropped_jpeg` re-encodes the image with the crop actually applied
    to the pixels.  EXIF and the ICC profile are carried across; the EXIF
    Orientation tag is reset to 1 because the crop is computed against the
    already-rotated image, so the output must not be rotated a second time.

RAW
    Cropped pixels cannot be written back into a CR3/NEF/ARW — the file holds
    undemosaiced sensor data in a proprietary container, and re-encoding it
    would destroy the very thing a RAW is for.  Instead the RAW is copied
    untouched and `write_xmp_crop` drops an XMP sidecar beside it carrying the
    same normalised rect in Camera Raw's `crs:Crop*` fields.  Lightroom,
    Camera Raw and Bridge read that sidecar and open the file already cropped;
    anything that ignores XMP still sees a pristine RAW.

An existing sidecar at the destination is *merged*, not replaced, so develop
settings written by other software survive a re-export.
"""
from __future__ import annotations

import os
import xml.etree.ElementTree as ET
from datetime import datetime
from pathlib import Path
from typing import Optional

from app.models.crop import Crop

# Re-encode quality for cropped JPEGs.  95 with 4:4:4 chroma is visually
# indistinguishable from the camera original at normal viewing sizes.
JPEG_QUALITY     = 95
JPEG_SUBSAMPLING = 0        # 4:4:4 — no chroma decimation

try:
    from PIL import Image as _Image, ImageOps as _ImageOps
    _PIL_AVAILABLE = True
except ImportError:
    _PIL_AVAILABLE = False


class CropError(RuntimeError):
    """Raised when a crop could not be applied to a file."""


# ── JPEG ───────────────────────────────────────────────────────────────────── #

def render_cropped_jpeg(
    src:  Path,
    dest: Path,
    crop: Crop,
    quality: int = JPEG_QUALITY,
) -> None:
    """
    Write *src* to *dest* with *crop* applied to the pixels.

    Writes via a ``.~tmp`` sibling and os.replace()s it into position, so a
    failure part-way through never leaves a half-written JPEG at *dest*.
    """
    if not _PIL_AVAILABLE:
        raise CropError("Pillow is required to export cropped JPEGs")

    tmp = dest.with_suffix(dest.suffix + ".~tmp")
    try:
        with _Image.open(src) as img:
            exif         = img.getexif()
            icc          = img.info.get("icc_profile")
            # Crops are authored against the image as displayed, so rotate
            # first and let the box apply to the upright frame.
            upright      = _ImageOps.exif_transpose(img)
            box          = crop.to_pixels(upright.width, upright.height)
            cropped      = upright.crop(box)

            if cropped.mode not in ("RGB", "L", "CMYK"):
                cropped = cropped.convert("RGB")

            # The pixels are now upright and a different size; the old tags
            # would make downstream readers rotate or mis-report the file.
            exif[0x0112] = 1                 # Orientation: normal
            exif[0x0100] = cropped.width     # ImageWidth
            exif[0x0101] = cropped.height    # ImageLength

            save_kwargs = {
                "quality":     quality,
                "subsampling": JPEG_SUBSAMPLING,
                "optimize":    True,
                "exif":        exif.tobytes(),
            }
            if icc:
                save_kwargs["icc_profile"] = icc

            dest.parent.mkdir(parents=True, exist_ok=True)
            cropped.save(tmp, "JPEG", **save_kwargs)

        os.replace(str(tmp), str(dest))
    except Exception as exc:
        Path(tmp).unlink(missing_ok=True)
        if isinstance(exc, CropError):
            raise
        raise CropError(f"{src.name}: {exc}") from exc


# ── RAW / XMP ──────────────────────────────────────────────────────────────── #

_NS = {
    "x":   "adobe:ns:meta/",
    "rdf": "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
    "crs": "http://ns.adobe.com/camera-raw-settings/1.0/",
    "xmp": "http://ns.adobe.com/xap/1.0/",
}

for _prefix, _uri in _NS.items():
    ET.register_namespace(_prefix, _uri)

_RDF_DESC = f"{{{_NS['rdf']}}}Description"

_EMPTY_XMP = (
    '<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="photo-pipeline">'
    '<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">'
    '<rdf:Description rdf:about=""'
    ' xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"'
    ' xmlns:xmp="http://ns.adobe.com/xap/1.0/"/>'
    '</rdf:RDF></x:xmpmeta>'
)

_PACKET_HEAD = '<?xpacket begin="﻿" id="W5M0MpCehiHzreSzNTczkc9d"?>\n'
_PACKET_TAIL = '\n<?xpacket end="w"?>\n'


def xmp_sidecar_path(image_path: Path) -> Path:
    """Sidecar location for *image_path* — `IMG_0042.cr3` → `IMG_0042.xmp`."""
    return image_path.with_suffix(".xmp")


def write_xmp_crop(image_path: Path, crop: Crop) -> Path:
    """
    Write (or update) the XMP sidecar next to *image_path* with *crop*.

    Returns the sidecar path.  An existing sidecar has only its `crs:Crop*`
    fields rewritten — every other property it carries is preserved.  If the
    existing file cannot be parsed it is replaced rather than abandoned, on the
    grounds that an unreadable sidecar is not carrying anything worth keeping.
    """
    sidecar = xmp_sidecar_path(image_path)

    root: Optional[ET.Element] = None
    if sidecar.exists():
        try:
            root = ET.fromstring(sidecar.read_text(encoding="utf-8").strip()
                                 .lstrip("﻿"))
        except Exception:
            root = None
    if root is None:
        root = ET.fromstring(_EMPTY_XMP)

    desc = root.find(f".//{_RDF_DESC}")
    if desc is None:
        root = ET.fromstring(_EMPTY_XMP)
        desc = root.find(f".//{_RDF_DESC}")

    def crs(name: str, value: str) -> None:
        desc.set(f"{{{_NS['crs']}}}{name}", value)

    crs("HasCrop",            "True")
    crs("CropTop",            f"{crop.top:.6f}")
    crs("CropLeft",           f"{crop.left:.6f}")
    crs("CropBottom",         f"{crop.bottom:.6f}")
    crs("CropRight",          f"{crop.right:.6f}")
    crs("CropAngle",          "0")
    crs("CropConstrainToWarp", "1")
    desc.set(f"{{{_NS['xmp']}}}ModifyDate", datetime.now().isoformat(timespec="seconds"))
    desc.set(f"{{{_NS['xmp']}}}CreatorTool", "photo-pipeline")

    body = ET.tostring(root, encoding="unicode")
    tmp  = sidecar.with_suffix(sidecar.suffix + ".~tmp")
    try:
        sidecar.parent.mkdir(parents=True, exist_ok=True)
        tmp.write_text(_PACKET_HEAD + body + _PACKET_TAIL, encoding="utf-8")
        os.replace(str(tmp), str(sidecar))
    except Exception as exc:
        tmp.unlink(missing_ok=True)
        raise CropError(f"{sidecar.name}: {exc}") from exc
    return sidecar


def remove_xmp_crop(image_path: Path) -> None:
    """
    Clear the crop from an existing sidecar (set `crs:HasCrop` false).

    Used when a file that was previously exported cropped is re-exported
    uncropped — leaving the old rect behind would silently re-crop it.
    The sidecar is left in place so other develop settings survive; if there
    is no sidecar there is nothing to clear.
    """
    sidecar = xmp_sidecar_path(image_path)
    if not sidecar.exists():
        return
    try:
        root = ET.fromstring(sidecar.read_text(encoding="utf-8").strip().lstrip("﻿"))
        desc = root.find(f".//{_RDF_DESC}")
        if desc is None:
            return
        desc.set(f"{{{_NS['crs']}}}HasCrop", "False")
        for name in ("CropTop", "CropLeft", "CropBottom", "CropRight"):
            desc.attrib.pop(f"{{{_NS['crs']}}}{name}", None)
        tmp = sidecar.with_suffix(sidecar.suffix + ".~tmp")
        tmp.write_text(
            _PACKET_HEAD + ET.tostring(root, encoding="unicode") + _PACKET_TAIL,
            encoding="utf-8",
        )
        os.replace(str(tmp), str(sidecar))
    except Exception:
        # Best-effort: a sidecar we cannot rewrite is not worth failing an
        # otherwise-successful export over.
        pass
