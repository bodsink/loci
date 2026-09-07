#!/usr/bin/env python3
"""Compose docs/social-preview.png (1280x640) from the UI screenshot and mark."""

from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

ROOT = Path(__file__).resolve().parents[1]
SHOT = ROOT / "docs" / "loci-ui.jpg"
OUT = ROOT / "docs" / "social-preview.png"
W, H = 1280, 640
BG = (7, 7, 12, 255)
TEXT = (232, 234, 244, 255)
MUTED = (139, 144, 167, 255)
TEAL = (94, 234, 212, 255)


def font(size: int, bold: bool = False) -> ImageFont.FreeTypeFont:
    candidates = [
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf" if bold else "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf" if bold else "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    ]
    for path in candidates:
        if Path(path).exists():
            return ImageFont.truetype(path, size)
    return ImageFont.load_default()


def rounded(im: Image.Image, radius: int) -> Image.Image:
    mask = Image.new("L", im.size, 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, im.size[0], im.size[1]), radius, fill=255)
    out = im.convert("RGBA")
    out.putalpha(mask)
    return out


def main() -> None:
    canvas = Image.new("RGBA", (W, H), BG)
    draw = ImageDraw.Draw(canvas)

    # Soft orbs, matching the UI.
    for box, color in (
        ((380, -220, 980, 380), (45, 212, 191, 70)),
        ((900, 260, 1500, 860), (124, 58, 237, 70)),
    ):
        orb = Image.new("RGBA", (W, H), (0, 0, 0, 0))
        ImageDraw.Draw(orb).ellipse(box, fill=color)
        canvas = Image.alpha_composite(canvas, orb.filter(ImageFilter.GaussianBlur(90)))
    draw = ImageDraw.Draw(canvas)

    left = 64
    draw.text((left, 88), "Loci", font=font(68, bold=True), fill=TEXT)
    draw.text((left, 176), "Local-first code", font=font(26, bold=True), fill=TEAL)
    draw.text((left, 214), "intelligence for", font=font(26, bold=True), fill=TEAL)
    draw.text((left, 252), "AI coding agents.", font=font(26, bold=True), fill=TEAL)

    for i, line in enumerate(
        (
            "Persistent graph on disk",
            "15 MCP tools for Cursor",
            "No cloud. No API key.",
        )
    ):
        draw.text((left, 330 + i * 32), line, font=font(18), fill=MUTED)
    draw.text((left, 560), "Linux x86_64  ·  Apache-2.0", font=font(16), fill=MUTED)

    shot = Image.open(SHOT).convert("RGBA")
    # Crop past the project rail so the atlas, not the local folder name, is visible.
    cw, ch = shot.size
    crop = shot.crop((int(cw * 0.22), int(ch * 0.06), int(cw * 0.99), int(ch * 0.94)))
    crop.thumbnail((760, 500), Image.Resampling.LANCZOS)
    crop = rounded(crop, 22)

    shadow = Image.new("RGBA", (crop.size[0] + 40, crop.size[1] + 40), (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle(
        (10, 14, crop.size[0] + 30, crop.size[1] + 34), 26, fill=(0, 0, 0, 140)
    )
    shadow = shadow.filter(ImageFilter.GaussianBlur(16))
    x = W - crop.size[0] - 48
    canvas.alpha_composite(shadow, (x - 10, 70))
    canvas.alpha_composite(crop, (x, 64))

    canvas.convert("RGB").save(OUT, "PNG", optimize=True)
    print(f"wrote {OUT} ({OUT.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
