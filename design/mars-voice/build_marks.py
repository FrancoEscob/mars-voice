#!/usr/bin/env python3
"""Mars voice marks: the Mars SaaS disc (flat orange, a shadow made of dark
specks that thicken toward the limb, the dark orbiter) as the head of a
microphone. Deterministic; writes into crates/dicta-ui/assets/marks and
design/mars-voice/icons. Usage: python3 -I build_marks.py"""
import math, random, pathlib, subprocess

ROOT = pathlib.Path(__file__).resolve().parents[2]
MARKS = ROOT / "crates/dicta-ui/assets/marks"
ICONS = ROOT / "design/mars-voice/icons"
ORANGE, DEEP, INK, PALE = "#FF4F00", "#8C2400", "#16130F", "#E8EEE2"
SPECK = "#1A0D06"


def smooth(a, b, x):
    t = min(max((x - a) / (b - a), 0.0), 1.0)
    return t * t * (3 - 2 * t)


def specks(cx, cy, r, count, rmin, rmax, seed, min_gap):
    """Blue-noise specks inside the disc; density and size follow the
    shadow side (light from the upper left)."""
    rng = random.Random(seed)
    pts = []
    tries = 0
    while len(pts) < count and tries < count * 400:
        tries += 1
        a, d = rng.uniform(0, math.tau), r * math.sqrt(rng.random())
        x, y = cx + d * math.cos(a), cy + d * math.sin(a)
        s = ((x - cx) * 0.62 + (y - cy) * 0.78) / r
        limb = (math.hypot(x - cx, y - cy) / r) ** 3
        p = smooth(-0.15, 0.9, s) ** 1.3 * 0.97 + 0.12 * limb * smooth(-0.6, 0.2, s)
        if rng.random() > p:
            continue
        rad = rmin + (rmax - rmin) * smooth(-0.1, 1.0, s) * rng.uniform(0.75, 1.0)
        if any(math.hypot(x - px, y - py) < (rad + pr) + min_gap for px, py, pr in pts):
            continue
        pts.append((x, y, rad))
    return "".join(
        f"M{x - q:.2f} {y:.2f}a{q:.2f} {q:.2f} 0 1 0 {2*q:.2f} 0a{q:.2f} {q:.2f} 0 1 0 {-2*q:.2f} 0"
        for x, y, q in pts
    )


def planet(cx, cy, r, detail, uid):
    """Disc + speck shadow (+ fine grain at the large size)."""
    grad = (f'<radialGradient id="g{uid}" cx=".34" cy=".3" r=".95"><stop offset="0" stop-color="#FF7433"/>'
            f'<stop offset=".5" stop-color="{ORANGE}"/><stop offset="1" stop-color="#C83A00"/></radialGradient>')
    clip = f'<clipPath id="c{uid}"><circle cx="{cx}" cy="{cy}" r="{r}"/></clipPath>'
    body = f'<circle cx="{cx}" cy="{cy}" r="{r}" fill="url(#g{uid})"/>'
    if detail == "large":
        grain = (f'<filter id="n{uid}" x="0" y="0" width="1" height="1" color-interpolation-filters="sRGB">'
                 f'<feTurbulence type="fractalNoise" baseFrequency="2.4" numOctaves="1" seed="7"/>'
                 f'<feColorMatrix values="0 0 0 0 .2  0 0 0 0 .05  0 0 0 0 0  .9 0 0 0 -.5"/></filter>')
        d = specks(cx, cy, r, 190, r * 0.02, r * 0.05, 11, r * 0.014)
        return (grad + clip + grain,
                body + f'<g clip-path="url(#c{uid})"><rect x="{cx-r}" y="{cy-r}" width="{2*r}" height="{2*r}" filter="url(#n{uid})"/>'
                f'<path fill="{SPECK}" d="{d}"/></g>')
    if detail == "medium":
        d = specks(cx, cy, r, 34, r * 0.05, r * 0.11, 5, r * 0.03)
        return grad + clip, body + f'<path clip-path="url(#c{uid})" fill="{SPECK}" d="{d}"/>'
    # Favicon grade: a solid shadow crescent instead of specks.
    return (clip, f'<circle cx="{cx}" cy="{cy}" r="{r}" fill="{ORANGE}"/>'
            f'<circle clip-path="url(#c{uid})" cx="{cx + r*0.55}" cy="{cy + r*0.69}" r="{r*1.05}" fill="{DEEP}"/>'
            f'<circle clip-path="url(#c{uid})" cx="{cx - r*0.08}" cy="{cy - r*0.1}" r="{r*0.98}" fill="{ORANGE}"/>')


def mark(size, ink, detail, uid="m"):
    """64-unit microphone: planet head, cradle, stem, base and orbiter."""
    defs, head = planet(32, 25, 16, detail, uid)
    w = {"large": 3.4, "medium": 3.6, "small": 4.6}[detail]
    cradle = ('<path d="M12.5 26.5a19.5 19.5 0 0 0 39 0"/><path d="M32 46v9M24.5 56h15"/>')
    orbiter = f'<circle cx="53.5" cy="8.5" r="{3.6 if detail != "small" else 4.6}" fill="{ink}"/>'
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" width="{size}" height="{size}" role="img" aria-label="Mars voice">'
            f'<defs>{defs}</defs>{head}'
            f'<g fill="none" stroke="{ink}" stroke-width="{w}" stroke-linecap="round">{cradle}</g>{orbiter}</svg>')


def planet_only(detail, uid="p"):
    """Mars alone (overlay mic, Órbita centre): 26-unit box."""
    defs, head = planet(13, 13, 12.4, detail, uid)
    return f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 26 26"><defs>{defs}</defs>{head}</svg>'


def launcher(bg, ink, uid):
    """Square app icon: the mark on a rounded tile."""
    inner = mark(64, ink, "large", uid)
    inner = inner.split(">", 1)[1].rsplit("</svg>", 1)[0]
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256" role="img" aria-label="Mars voice">'
            f'<rect x="8" y="8" width="240" height="240" rx="56" fill="{bg}"/>'
            f'<g transform="translate(36 34) scale(2.9)">{inner}</g></svg>')


def main():
    MARKS.mkdir(parents=True, exist_ok=True)
    ICONS.mkdir(parents=True, exist_ok=True)
    out = {
        MARKS / "mars-voice.svg": mark(64, INK, "large"),
        MARKS / "mars-voice-dark.svg": mark(64, PALE, "large"),
        MARKS / "mars-voice-32.svg": mark(32, INK, "medium"),
        MARKS / "mars-voice-32-dark.svg": mark(32, PALE, "medium"),
        MARKS / "mars-voice-16.svg": mark(16, INK, "small"),
        MARKS / "mars-voice-16-dark.svg": mark(16, PALE, "small"),
        MARKS / "mars-planet.svg": planet_only("large"),
        MARKS / "mars-planet-small.svg": planet_only("medium"),
        ICONS / "mars-voice-launcher.svg": launcher("#EEF4E6", INK, "l"),
        ICONS / "mars-voice-launcher-dark.svg": launcher("#1B1A17", PALE, "d"),
        ICONS / "mars-voice-symbolic-16.svg": mark(16, INK, "small"),
    }
    for path, svg in out.items():
        path.write_text(svg + "\n")
    for px in (512, 256, 128, 64, 48, 32):
        subprocess.run(["rsvg-convert", "-w", str(px), "-h", str(px), "-o",
                        str(ICONS / f"mars-voice-{px}.png"), str(ICONS / "mars-voice-launcher.svg")], check=True)


if __name__ == "__main__":
    main()
