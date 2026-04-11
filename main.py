"""
Entry point for the Photo Pipeline desktop application.

Usage:
    python main.py
"""
import sys

from PySide6.QtCore import Qt
from PySide6.QtGui import QColor, QImageReader, QPalette
from PySide6.QtWidgets import QApplication

from app.main_window import MainWindow


# ──────────────────────────────────────────────────────────────────────────────
# Theme
# ──────────────────────────────────────────────────────────────────────────────

def _apply_dark_palette(app: QApplication) -> None:
    """
    Palette inspired by bealsbe.github.io:
      background  #0a0a12  deep blue-black
      accent      #ff6d00  orange
      text        #f0f0f0  near-white
    """
    app.setStyle("Fusion")

    p = QPalette()
    bg       = QColor(0x0a, 0x0a, 0x12)   # #0a0a12
    surface  = QColor(0x0e, 0x0e, 0x1a)   # #0e0e1a
    card     = QColor(0x13, 0x13, 0x1f)   # #13131f
    text     = QColor(0xf0, 0xf0, 0xf0)   # #f0f0f0
    accent   = QColor(0xff, 0x6d, 0x00)   # #ff6d00
    btn      = QColor(0x19, 0x19, 0x28)   # #191928
    dim_text = QColor(0x88, 0x88, 0xa8)   # muted text

    p.setColor(QPalette.Window,           bg)
    p.setColor(QPalette.WindowText,       text)
    p.setColor(QPalette.Base,             surface)
    p.setColor(QPalette.AlternateBase,    card)
    p.setColor(QPalette.Text,             text)
    p.setColor(QPalette.Button,           btn)
    p.setColor(QPalette.ButtonText,       text)
    p.setColor(QPalette.Highlight,        accent)
    p.setColor(QPalette.HighlightedText,  QColor(255, 255, 255))
    p.setColor(QPalette.ToolTipBase,      QColor(0x16, 0x16, 0x24))
    p.setColor(QPalette.ToolTipText,      text)
    p.setColor(QPalette.Link,             accent)
    p.setColor(QPalette.BrightText,       QColor(255, 80, 80))

    p.setColor(QPalette.Disabled, QPalette.Text,       dim_text)
    p.setColor(QPalette.Disabled, QPalette.ButtonText, dim_text)

    app.setPalette(p)


# ──────────────────────────────────────────────────────────────────────────────
# Main
# ──────────────────────────────────────────────────────────────────────────────

def main() -> None:
    # Enable HiDPI scaling on Qt 5-compat paths (no-op on Qt 6)
    QApplication.setHighDpiScaleFactorRoundingPolicy(
        Qt.HighDpiScaleFactorRoundingPolicy.PassThrough
    )

    app = QApplication(sys.argv)
    app.setApplicationName("Photo Pipeline")
    app.setOrganizationName("PhotoPipeline")

    # Disable the 256 MB allocation guard — we read large source images
    # (RAW/high-res JPEG) and rely on QImageReader's setScaledSize to
    # produce small thumbnails, so the guard fires spuriously.
    QImageReader.setAllocationLimit(0)

    _apply_dark_palette(app)

    window = MainWindow()
    window.show()

    sys.exit(app.exec())


if __name__ == "__main__":
    main()
