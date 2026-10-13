# Regenerates the grain tiles:
#   python3 generate.py OUT_DIR [STRENGTH=0.42] [REGOLITH_FINE=0.30] [REGOLITH_CLUMPS=0.36]
# Each tile is a 256 px stitched fractal noise, so tiles repeat seamlessly.
import sys

out = sys.argv[1]
# Tuned on the real GPUI (resvg) render: stronger washes over the text.
A = float(sys.argv[2]) if len(sys.argv) > 2 else 0.42
# Regolito, a step clearer than v3 (0.42 both): the fine speckle is what
# muddies letters, the clumps carry the regolith, so the speckle drops more.
RF = float(sys.argv[3]) if len(sys.argv) > 3 else 0.30
RC = float(sys.argv[4]) if len(sys.argv) > 4 else 0.36


def tile(name, ink, pale, freq, octaves, k_ink, b_ink, k_pale, b_pale, seed, coarse=None):
    def matrix(rgb, k, b, channel):
        r, g, bb = rgb
        row = [0, 0, 0, 0]
        row[channel] = k
        return f"0 0 0 0 {r:.3f}  0 0 0 0 {g:.3f}  0 0 0 0 {bb:.3f}  {' '.join(f'{v:.3f}' for v in row)} {b:.3f}"

    layers = f'''<feTurbulence type="fractalNoise" baseFrequency="{freq}" numOctaves="{octaves}" seed="{seed}" stitchTiles="stitch" result="t"/>
<feColorMatrix in="t" values="{matrix(ink, k_ink, b_ink, 0)}" result="dark"/>
<feColorMatrix in="t" values="{matrix(pale, k_pale, b_pale, 1)}" result="light"/>'''
    merge = '<feMergeNode in="light"/><feMergeNode in="dark"/>'
    if coarse:
        cf, ck, cb, cs = coarse
        layers += f'''
<feTurbulence type="fractalNoise" baseFrequency="{cf}" numOctaves="2" seed="{cs}" stitchTiles="stitch" result="t2"/>
<feColorMatrix in="t2" values="{matrix(ink, ck, cb, 0)}" result="clumps"/>'''
        merge = '<feMergeNode in="clumps"/>' + merge
    svg = f'''<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256" viewBox="0 0 256 256"><filter id="n" x="0" y="0" width="1" height="1" color-interpolation-filters="sRGB">
{layers}
<feMerge>{merge}</feMerge></filter><rect width="256" height="256" filter="url(#n)"/></svg>
'''
    open(f"{out}/{name}.svg", "w").write(svg)


ink = (0x24 / 255, 0x27 / 255, 0x1E / 255)
white = (1, 1, 0.97)
pale = (0xCF / 255, 0xE3 / 255, 0xC4 / 255)
black = (0, 0, 0)
# Señal: fine ink specks plus paper flecks.
tile("grain-fine", ink, white, 0.9, 3, 1.7 * A, -0.78 * A, 1.5 * A, -0.72 * A, 11)
# Regolito: the same fine grain over soft regolith clumps.
tile("grain-coarse", ink, white, 0.8, 3, 1.9 * RF, -0.86 * RF, 1.6 * RF, -0.76 * RF, 7,
     coarse=(0.07, 0.55 * RC, -0.24 * RC, 3))
# Noche: pale specks on charcoal plus darker pits.
tile("grain-night", black, pale, 0.9, 3, 1.8 * A, -0.82 * A, 1.9 * A, -0.9 * A, 11)
# Regolito sobre carbón: the same soft clumps, dark, with pale specks.
tile("grain-coarse-night", black, pale, 0.8, 3, 1.9 * RF, -0.86 * RF, 1.9 * RF, -0.9 * RF, 7,
     coarse=(0.07, 0.6 * RC, -0.24 * RC, 3))
