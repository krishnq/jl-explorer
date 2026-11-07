"""Draw the JL Explorer app icon: a happy little speaker on a gradient tile.

Usage: python make_icon.py <out_dir> [preview.png]
Writes icon.png (256 px) and icon.ico (16-256 px) into <out_dir>. Needs Pillow.
"""
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

S = 1024  # drawing resolution; downscaled for smooth edges

TOP = (110, 168, 255)
BOTTOM = (139, 108, 242)
CREAM = (255, 246, 236)
NAVY = (36, 44, 86)
BLUE = (79, 163, 247)
LIGHT_BLUE = (138, 196, 255)
PINK = (255, 143, 177)
WHITE = (255, 255, 255)


def rounded_mask(box, radius):
    m = Image.new("L", (S, S), 0)
    ImageDraw.Draw(m).rounded_rectangle(box, radius, fill=255)
    return m


def circle(d, cx, cy, r, fill):
    d.ellipse((cx - r, cy - r, cx + r, cy + r), fill=fill)


def wave(d, cx, cy, r, width, color):
    """Sound-wave arc with rounded ends."""
    box = (cx - r, cy - r, cx + r, cy + r)
    d.arc(box, -42, 42, fill=color, width=width)
    import math
    for a in (-42, 42):
        rr = r - width / 2
        x, y = cx + rr * math.cos(math.radians(a)), cy + rr * math.sin(math.radians(a))
        circle(d, x, y, width / 2, color)


def sparkle(d, cx, cy, r, color):
    w = r * 0.28
    d.polygon([(cx, cy - r), (cx + w, cy - w), (cx + r, cy), (cx + w, cy + w), (cx, cy + r), (cx - w, cy + w), (cx - r, cy), (cx - w, cy - w)], fill=color)


def draw():
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))

    # gradient tile
    grad = Image.new("RGBA", (S, S))
    gd = ImageDraw.Draw(grad)
    for y in range(S):
        t = y / (S - 1)
        gd.line([(0, y), (S, y)], fill=tuple(round(a + (b - a) * t) for a, b in zip(TOP, BOTTOM)) + (255,))
    tile = (40, 40, S - 40, S - 40)
    img.paste(grad, (0, 0), rounded_mask(tile, 230))

    # soft highlight on the upper part of the tile
    hl = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ImageDraw.Draw(hl).ellipse((-200, -520, S + 200, 430), fill=(255, 255, 255, 38))
    img.alpha_composite(Image.composite(hl, Image.new("RGBA", (S, S), (0, 0, 0, 0)), rounded_mask(tile, 230)))

    # speaker body with a soft shadow
    body = (250, 190, 690, 860)
    shadow = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle((body[0], body[1] + 26, body[2], body[3] + 26), 120, fill=(30, 20, 80, 90))
    img.alpha_composite(shadow.filter(ImageFilter.GaussianBlur(22)))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle(body, 120, fill=CREAM)

    # tweeter
    circle(d, 470, 330, 66, NAVY)
    circle(d, 470, 330, 46, BLUE)
    circle(d, 455, 314, 13, WHITE)

    # woofer with a face
    cx, cy = 470, 628
    circle(d, cx, cy, 180, NAVY)
    circle(d, cx, cy, 154, BLUE)
    circle(d, cx, cy - 6, 128, LIGHT_BLUE)
    for ex in (cx - 56, cx + 56):
        d.ellipse((ex - 20, cy - 62, ex + 20, cy - 10), fill=NAVY)
        circle(d, ex - 7, cy - 48, 8, WHITE)
    blush = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    bd = ImageDraw.Draw(blush)
    for bx in (cx - 92, cx + 92):
        bd.ellipse((bx - 28, cy + 2, bx + 28, cy + 30), fill=PINK + (200,))
    img.alpha_composite(blush.filter(ImageFilter.GaussianBlur(3)))
    d = ImageDraw.Draw(img)
    d.arc((cx - 42, cy - 26, cx + 42, cy + 44), 25, 155, fill=NAVY, width=13)

    # sound waves and sparkles, on their own layer so translucent white blends
    fx = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    fd = ImageDraw.Draw(fx)
    wave(fd, 600, 560, 170, 34, WHITE + (240,))
    wave(fd, 600, 560, 262, 34, WHITE + (150,))
    sparkle(fd, 838, 236, 46, WHITE + (235,))
    sparkle(fd, 168, 846, 26, WHITE + (160,))
    img.alpha_composite(fx)
    return img


def main():
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    big = draw()
    big.resize((256, 256), Image.LANCZOS).save(out / "icon.png")
    sizes = [16, 24, 32, 48, 64, 128, 256]
    big.resize((256, 256), Image.LANCZOS).save(out / "icon.ico", sizes=[(s, s) for s in sizes])
    if len(sys.argv) > 2:
        # preview sheet: large plus the small sizes Windows shows
        sheet = Image.new("RGBA", (700, 520), (32, 34, 40, 255))
        sheet.alpha_composite(big.resize((512, 512), Image.LANCZOS), (4, 4))
        y = 10
        for s in (128, 64, 48, 32, 24, 16):
            sheet.alpha_composite(big.resize((s, s), Image.LANCZOS), (540, y))
            y += s + 10
        sheet.save(sys.argv[2])


if __name__ == "__main__":
    main()
