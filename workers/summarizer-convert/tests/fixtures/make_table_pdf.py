"""Writes a PDF whose pages hold text lines and ruled grid tables.

Usage: make_table_pdf.py OUT ITEM ...

An ITEM of `--` starts a new page. An ITEM holding `;` is one grid row, its
cells split at `;`; consecutive rows form one ruled table. Any other ITEM is
a text line. Standard library only.
"""
import sys

out, items = sys.argv[1], sys.argv[2:]
pages = [[]]
for item in items:
    if item == "--":
        pages.append([])
    else:
        pages[-1].append(item)


def esc(s):
    return s.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")


def page_ops(lines):
    ops, y, grid = ["0.5 w"], 740, []

    def flush():
        nonlocal y, grid
        if not grid:
            return
        cols, w, h, x0 = max(len(r) for r in grid), 200, 24, 72
        top = y + 10
        for r in range(len(grid) + 1):
            ops.append(f"{x0} {top - r * h} m {x0 + cols * w} {top - r * h} l S")
        for c in range(cols + 1):
            x = x0 + c * w
            ops.append(f"{x} {top} m {x} {top - len(grid) * h} l S")
        for r, row in enumerate(grid):
            for c, cell in enumerate(row):
                ops.append(f"BT /F1 12 Tf {x0 + c * w + 6} {top - r * h - 17} Td ({esc(cell)}) Tj ET")
        y = top - len(grid) * h - 30
        grid = []

    for line in lines:
        if ";" in line:
            grid.append(line.split(";"))
            continue
        flush()
        ops.append(f"BT /F1 12 Tf 72 {y} Td ({esc(line)}) Tj ET")
        y -= 28
    flush()
    return "\n".join(ops).encode()


objs = {1: b"<< /Type /Catalog /Pages 2 0 R >>",
        3: b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"}
kids, n = [], 4
for lines in pages:
    stream = page_ops(lines)
    objs[n] = (b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
               b"/Resources << /Font << /F1 3 0 R >> >> /Contents %d 0 R >>" % (n + 1))
    objs[n + 1] = b"<< /Length %d >>\nstream\n" % len(stream) + stream + b"\nendstream"
    kids.append(n)
    n += 2
objs[2] = b"<< /Type /Pages /Kids [%s] /Count %d >>" % (
    b" ".join(b"%d 0 R" % k for k in kids), len(kids))
data = bytearray(b"%PDF-1.4\n")
offsets = {}
for i in sorted(objs):
    offsets[i] = len(data)
    data += b"%d 0 obj\n" % i + objs[i] + b"\nendobj\n"
xref = len(data)
data += b"xref\n0 %d\n0000000000 65535 f \n" % n
for i in range(1, n):
    data += b"%010d 00000 n \n" % offsets[i]
data += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (n, xref)
open(out, "wb").write(data)
