#!/usr/bin/env python3
"""Writes survey.xlsx: a two-sheet workbook made for this project's tests.

Standard library only (zipfile); fixed timestamps, so a rerun gives the same
bytes. Sheet "Lights" holds the harbor survey counts, sheet "Keepers" the
lamp checks. All text is written for this project.

Usage: python3 make_xlsx.py survey.xlsx
"""
import sys
import zipfile
from xml.sax.saxutils import escape

SHEETS = [
    (
        "Lights",
        [
            ["Pier", "Lights", "Dim", "Dark", "Note"],
            ["North", 42, 7, 3, "forty two lights, three were dark"],
            ["South", 18, 2, 0, "all lamps lit at dawn"],
        ],
    ),
    (
        "Keepers",
        [
            ["Check", "Time", "Result"],
            ["Dusk round", "18:30", "replaced every dark lamp"],
            ["Dawn round", "06:15", "no lamp out"],
        ],
    ),
]

strings = []


def sid(text):
    if text not in strings:
        strings.append(text)
    return strings.index(text)


def col(i):
    return "ABCDEFGHIJ"[i]


def sheet_xml(rows):
    out = []
    for r, row in enumerate(rows, start=1):
        cells = []
        for c, value in enumerate(row):
            ref = f"{col(c)}{r}"
            if isinstance(value, int):
                cells.append(f'<c r="{ref}"><v>{value}</v></c>')
            else:
                cells.append(f'<c r="{ref}" t="s"><v>{sid(value)}</v></c>')
        out.append(f'<row r="{r}">{"".join(cells)}</row>')
    return (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
        '<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">'
        f'<sheetData>{"".join(out)}</sheetData></worksheet>'
    )


def main(path):
    sheets = [sheet_xml(rows) for _, rows in SHEETS]
    ns = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
    rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
    files = {
        "[Content_Types].xml": (
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
            '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
            '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
            '<Default Extension="xml" ContentType="application/xml"/>'
            '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
            + "".join(
                f'<Override PartName="/xl/worksheets/sheet{i}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
                for i in range(1, len(SHEETS) + 1)
            )
            + '<Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>'
            "</Types>"
        ),
        "_rels/.rels": (
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
            '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
            f'<Relationship Id="rId1" Type="{rel}/officeDocument" Target="xl/workbook.xml"/>'
            "</Relationships>"
        ),
        "xl/workbook.xml": (
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
            f'<workbook xmlns="{ns}" xmlns:r="{rel}"><sheets>'
            + "".join(
                f'<sheet name="{name}" sheetId="{i}" r:id="rId{i}"/>'
                for i, (name, _) in enumerate(SHEETS, start=1)
            )
            + "</sheets></workbook>"
        ),
        "xl/_rels/workbook.xml.rels": (
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
            '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
            + "".join(
                f'<Relationship Id="rId{i}" Type="{rel}/worksheet" Target="worksheets/sheet{i}.xml"/>'
                for i in range(1, len(SHEETS) + 1)
            )
            + f'<Relationship Id="rId{len(SHEETS) + 1}" Type="{rel}/sharedStrings" Target="sharedStrings.xml"/>'
            "</Relationships>"
        ),
    }
    for i, xml in enumerate(sheets, start=1):
        files[f"xl/worksheets/sheet{i}.xml"] = xml
    files["xl/sharedStrings.xml"] = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
        f'<sst xmlns="{ns}" count="{len(strings)}" uniqueCount="{len(strings)}">'
        + "".join(f"<si><t>{escape(s)}</t></si>" for s in strings)
        + "</sst>"
    )
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        for name, text in files.items():
            info = zipfile.ZipInfo(name, date_time=(2026, 11, 14, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            z.writestr(info, text.encode("utf-8"))


if __name__ == "__main__":
    main(sys.argv[1])
