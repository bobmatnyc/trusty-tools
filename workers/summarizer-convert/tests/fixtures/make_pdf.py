"""Writes a minimal one-font PDF. Usage: make_pdf.py OUT [LINE ...].

No LINE gives a page with no text, as a scanned PDF would give.
"""
import sys

out, lines = sys.argv[1], sys.argv[2:]
ops = ["BT", "/F1 12 Tf", "72 720 Td", "14 TL"]
for line in lines:
    esc = line.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")
    ops.append(f"({esc}) Tj T*")
ops.append("ET")
stream = "\n".join(ops).encode() if lines else b""
objs = [
    b"<< /Type /Catalog /Pages 2 0 R >>",
    b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
    b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
    b"<< /Length %d >>\nstream\n" % len(stream) + stream + b"\nendstream",
    b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
]
data = bytearray(b"%PDF-1.4\n")
offsets = []
for i, body in enumerate(objs, 1):
    offsets.append(len(data))
    data += b"%d 0 obj\n" % i + body + b"\nendobj\n"
xref = len(data)
data += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objs) + 1)
for off in offsets:
    data += b"%010d 00000 n \n" % off
data += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objs) + 1, xref)
open(out, "wb").write(data)
