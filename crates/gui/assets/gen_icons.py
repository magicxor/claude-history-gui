"""Generate the app icons (DESIGN §6, §16.3): app.png (512 px), app.ico, app.icns.

Run from this directory:  python gen_icons.py   (needs Pillow; the outputs are committed,
so only whoever changes the artwork needs it).

The artwork is original: a rounded square with a violet gradient, two overlapping
speech bubbles (the conversations) and a small clock arc (the history).
"""

from PIL import Image, ImageChops, ImageDraw

SIZE = 1024  # drawn at 1024, scaled down for the outputs
SCALE = 2  # supersampling


def lerp(a, b, t):
    return tuple(int(a[i] + (b[i] - a[i]) * t) for i in range(3))


def gradient(size):
    top, bottom = (84, 70, 190), (36, 28, 104)
    image = Image.new("RGB", (size, size))
    px = image.load()
    for y in range(size):
        row = lerp(top, bottom, y / (size - 1))
        for x in range(size):
            px[x, y] = row
    return image


def bubble(draw, box, radius, tail, fill):
    draw.rounded_rectangle(box, radius=radius, fill=fill)
    draw.polygon(tail, fill=fill)


def artwork():
    s = SIZE * SCALE
    base = gradient(s)
    mask = Image.new("L", (s, s), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, s - 1, s - 1), radius=int(s * 0.22), fill=255)
    icon = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    icon.paste(base, (0, 0), mask)

    layer = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(layer)
    u = s / 100.0
    # Back bubble (translucent), front bubble (solid).
    bubble(
        d,
        (22 * u, 20 * u, 70 * u, 52 * u),
        6 * u,
        [(30 * u, 50 * u), (30 * u, 62 * u), (42 * u, 50 * u)],
        (255, 255, 255, 120),
    )
    bubble(
        d,
        (36 * u, 38 * u, 80 * u, 70 * u),
        6 * u,
        [(70 * u, 68 * u), (74 * u, 80 * u), (60 * u, 68 * u)],
        (255, 255, 255, 245),
    )
    # Text lines in the front bubble.
    for i, (right) in enumerate((70, 62, 66)):
        y = (46 + i * 7) * u
        d.rounded_rectangle((42 * u, y, right * u, y + 3.2 * u), radius=1.6 * u, fill=(60, 48, 150, 255))
    # History arc (a clock hand) in the top-right corner.
    cx, cy, r = 76 * u, 24 * u, 10 * u
    d.ellipse((cx - r, cy - r, cx + r, cy + r), outline=(255, 255, 255, 230), width=int(2.4 * u))
    d.line((cx, cy, cx, cy - 6 * u), fill=(255, 255, 255, 230), width=int(2.2 * u))
    d.line((cx, cy, cx + 4.5 * u, cy + 2 * u), fill=(255, 255, 255, 230), width=int(2.2 * u))
    icon = Image.alpha_composite(icon, layer)
    # Keep the corners transparent after compositing.
    alpha = ImageChops.multiply(icon.getchannel("A"), mask)
    icon.putalpha(alpha)
    return icon.resize((SIZE, SIZE), Image.LANCZOS)


def main():
    master = artwork()
    master.resize((512, 512), Image.LANCZOS).save("app.png", optimize=True)
    master.save("app.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
    master.save("app.icns", format="ICNS", sizes=[(16, 16), (32, 32), (64, 64), (128, 128), (256, 256), (512, 512), (1024, 1024)])


if __name__ == "__main__":
    main()
