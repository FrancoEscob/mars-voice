# Builds orbit.html, a single self-contained file: python3 build.py
# Fonts are Latin subsets of crates/dicta-ui/assets/fonts (OFL), made with:
#   uvx --from fonttools --with brotli pyftsubset FONT.ttf --flavor=woff2 \
#     --unicodes='U+0020-007E,U+00A0-00FF,U+2013-2014,U+2018-201D,U+2026,U+2192,U+2212,U+00B7,U+2248,U+2264,U+2265'
import base64
import pathlib

here = pathlib.Path(__file__).parent
html = (here / "orbit.src.html").read_text()
for font in sorted((here / "fonts").glob("*.woff2")):
    html = html.replace(f"@{font.stem}@", base64.b64encode(font.read_bytes()).decode())
assert "@Archivo" not in html and "@IBMPlex" not in html, "missing font"
(here / "orbit.html").write_text(html)
print(f"orbit.html: {len(html) // 1024} KB")
