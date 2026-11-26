#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["pillow==11.3.0"]
# ///
"""Render the DMG window background (1x and 2x) with install and first-launch hints.

Run after changing the text; the PNGs are committed so packaging needs no Pillow.
"""
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[1]
FONTS = ROOT / "crates/app/assets"
OUT = ROOT / "packaging/macos"
# Window content size in points; icon centres must match scripts/dmg-settings.py.
WIDTH, HEIGHT = 660, 420
APP_X, APPS_X, ICON_Y = 170, 490, 170
TEXT = (32, 33, 36)
MUTED = (96, 99, 105)
ACCENT = (46, 125, 50)


def render(scale: int) -> Image.Image:
    image = Image.new("RGB", (WIDTH * scale, HEIGHT * scale), (248, 248, 247))
    draw = ImageDraw.Draw(image)

    def font(name: str, size: int) -> ImageFont.FreeTypeFont:
        return ImageFont.truetype(str(FONTS / f"Inter-{name}.ttf"), size * scale)

    def centred(text: str, y: int, face: ImageFont.FreeTypeFont, fill) -> None:
        draw.text((WIDTH * scale // 2, y * scale), text, font=face, fill=fill, anchor="mt")

    centred("Перетащите notetaker в папку Applications", 36, font("SemiBold", 18), TEXT)
    # Arrow between the two icons.
    y = ICON_Y * scale
    left, right = (APP_X + 70) * scale, (APPS_X - 70) * scale
    draw.line((left, y, right - 14 * scale, y), fill=ACCENT, width=4 * scale)
    draw.polygon(
        [(right, y), (right - 18 * scale, y - 11 * scale), (right - 18 * scale, y + 11 * scale)],
        fill=ACCENT,
    )
    draw.line((40 * scale, 292 * scale, (WIDTH - 40) * scale, 292 * scale), fill=(222, 222, 220), width=scale)
    centred("Первый запуск", 308, font("SemiBold", 14), TEXT)
    body = font("Regular", 13)
    for index, line in enumerate(
        [
            "Откройте notetaker из папки Applications.",
            "Если macOS пишет, что приложение нельзя открыть: Системные настройки →",
            "Конфиденциальность и безопасность → «Всё равно открыть».",
        ]
    ):
        centred(line, 332 + index * 20, body, MUTED)
    return image


if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    render(1).save(OUT / "dmg-background.png", optimize=True)
    render(2).save(OUT / "dmg-background@2x.png", optimize=True)
