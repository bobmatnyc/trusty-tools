"""dashboard_toml.py — TOML reading for the dashboard on every supported Python (#10606).

Why: the dashboard reads Cargo.toml, pyproject.toml and .trusty-mpm.toml. `tomllib` exists from Python 3.11 only,
  and macOS ships 3.9 at /usr/bin/python3, so without a fallback the Projects stack and config rows go empty. The
  Architect is standard-library Python with nothing to install, so the fallback ships here as a seeded file.
What: loads(text) -> dict and TOMLDecodeError (a ValueError). On 3.11+ both are tomllib's own. Below 3.11 a
  pure-Python reader covers TOML 1.0 documents: tables, arrays of tables, dotted and quoted keys, basic, literal and
  multi-line strings, integers (decimal, hex, octal, binary), floats, booleans, arrays and inline tables. Dates and
  times come back as their source text, not datetime objects; the dashboard drops them either way.
Test: tests/test_dashboard_python_floor.py
"""
import re

try:
    from tomllib import TOMLDecodeError, loads
    NATIVE = True
except ImportError:   # Python < 3.11: the reader below (#10606)
    NATIVE = False

    class TOMLDecodeError(ValueError):
        """A document this reader cannot parse."""

    def loads(text):
        """Parse TOML text into a dict; raise TOMLDecodeError on malformed input."""
        if not isinstance(text, str):
            raise TypeError("loads() takes a str")
        return _Reader(text).document()


_BARE = re.compile(r"[A-Za-z0-9_-]+")
_NUMBER = re.compile(r"[+-]?(?:0x[0-9A-Fa-f_]+|0o[0-7_]+|0b[01_]+|inf|nan|[0-9_]+(?:\.[0-9_]+)?(?:[eE][+-]?[0-9_]+)?)")
_DATETIME = re.compile(r"\d{4}-\d{2}-\d{2}(?:[Tt ]\d{2}:\d{2}(?::\d{2}(?:\.\d+)?)?(?:[Zz]|[+-]\d{2}:\d{2})?)?|\d{2}:\d{2}:\d{2}(?:\.\d+)?")
_ESCAPES = {"b": "\b", "t": "\t", "n": "\n", "f": "\f", "r": "\r", '"': '"', "\\": "\\", "e": "\x1b"}


class _Reader:
    def __init__(self, text):
        self.s, self.i = text, 0
        self.root = {}
        self.defined = set()   # ids of tables opened by a [header], so a repeated header is an error

    def fail(self, msg):
        line = self.s.count("\n", 0, self.i) + 1
        raise TOMLDecodeError("%s (line %d)" % (msg, line))

    def peek(self, n=1):
        return self.s[self.i:self.i + n]

    def skip_ws(self):
        while self.i < len(self.s) and self.s[self.i] in " \t":
            self.i += 1

    def skip_comment(self):
        if self.peek() == "#":
            nl = self.s.find("\n", self.i)
            self.i = len(self.s) if nl < 0 else nl

    def skip_blank(self):
        """Whitespace, newlines and comments: the filler allowed between array elements."""
        while True:
            self.skip_ws()
            self.skip_comment()
            if self.peek() in ("\n", "\r") and self.peek() != "":
                self.i += 1
            else:
                return

    def end_of_line(self):
        self.skip_ws()
        self.skip_comment()
        if self.peek(2) == "\r\n":
            self.i += 2
        elif self.peek() == "\n":
            self.i += 1
        elif self.i < len(self.s):
            self.fail("expected end of line")

    def document(self):
        table = self.root
        while True:
            self.skip_blank()
            if self.i >= len(self.s):
                return self.root
            if self.peek(2) == "[[":
                self.i += 2
                keys = self.read_key()
                self.expect("]]")
                table = self.array_table(keys)
            elif self.peek() == "[":
                self.i += 1
                keys = self.read_key()
                self.expect("]")
                table = self.header_table(keys)
            else:
                self.pair(table)
            self.end_of_line()

    def expect(self, tok):
        self.skip_ws()
        if self.peek(len(tok)) != tok:
            self.fail("expected %r" % tok)
        self.i += len(tok)

    def read_key(self):
        parts = []
        while True:
            self.skip_ws()
            c = self.peek()
            if c == '"':
                parts.append(self.basic_string())
            elif c == "'":
                parts.append(self.literal_string())
            else:
                m = _BARE.match(self.s, self.i)
                if not m:
                    self.fail("expected a key")
                parts.append(m.group())
                self.i = m.end()
            self.skip_ws()
            if self.peek() != ".":
                return parts
            self.i += 1

    def descend(self, table, keys):
        for k in keys:
            nxt = table.setdefault(k, {})
            if isinstance(nxt, list) and nxt and isinstance(nxt[-1], dict):
                nxt = nxt[-1]
            if not isinstance(nxt, dict):
                self.fail("key %r is not a table" % k)
            table = nxt
        return table

    def header_table(self, keys):
        table = self.descend(self.root, keys)
        if id(table) in self.defined:
            self.fail("table %r defined twice" % ".".join(keys))
        self.defined.add(id(table))
        return table

    def array_table(self, keys):
        parent = self.descend(self.root, keys[:-1])
        arr = parent.setdefault(keys[-1], [])
        if not isinstance(arr, list):
            self.fail("key %r is not an array of tables" % ".".join(keys))
        arr.append({})
        return arr[-1]

    def pair(self, table):
        keys = self.read_key()
        self.expect("=")
        self.skip_ws()
        value = self.value()
        target = self.descend(table, keys[:-1])
        if keys[-1] in target:
            self.fail("key %r defined twice" % ".".join(keys))
        target[keys[-1]] = value

    def value(self):
        c = self.peek()
        if self.peek(3) == '"""':
            return self.multiline('"""')
        if self.peek(3) == "'''":
            return self.multiline("'''")
        if c == '"':
            return self.basic_string()
        if c == "'":
            return self.literal_string()
        if c == "[":
            return self.array()
        if c == "{":
            return self.inline_table()
        for word, val in (("true", True), ("false", False)):
            if self.s.startswith(word, self.i):
                self.i += len(word)
                return val
        m = _DATETIME.match(self.s, self.i)
        if m:
            self.i = m.end()
            return m.group()
        m = _NUMBER.match(self.s, self.i)
        if m and m.group():
            self.i = m.end()
            return self.number(m.group())
        self.fail("expected a value")

    def number(self, tok):
        t = tok.replace("_", "")
        sign, body = ("-" if t[0] == "-" else ""), t.lstrip("+-")
        if body in ("inf", "nan"):
            return float(sign + body)
        for prefix, base in (("0x", 16), ("0o", 8), ("0b", 2)):
            if body.startswith(prefix):
                return int(body[2:], base)
        try:
            return float(t) if any(ch in t for ch in ".eE") else int(t)
        except ValueError:
            self.fail("bad number %r" % tok)

    def escape(self):
        c = self.peek()
        self.i += 1
        if c in _ESCAPES:
            return _ESCAPES[c]
        width = {"u": 4, "U": 8}.get(c)
        if width:
            hexs = self.s[self.i:self.i + width]
            if len(hexs) != width or not re.fullmatch(r"[0-9A-Fa-f]+", hexs):
                self.fail("bad unicode escape")
            self.i += width
            return chr(int(hexs, 16))
        self.fail("bad escape \\%s" % c)

    def basic_string(self):
        self.i += 1
        out = []
        while True:
            c = self.peek()
            if c in ("", "\n"):
                self.fail("unterminated string")
            self.i += 1
            if c == '"':
                return "".join(out)
            out.append(self.escape() if c == "\\" else c)

    def literal_string(self):
        end = self.s.find("'", self.i + 1)
        nl = self.s.find("\n", self.i + 1)
        if end < 0 or 0 <= nl < end:
            self.fail("unterminated string")
        val, self.i = self.s[self.i + 1:end], end + 1
        return val

    def multiline(self, quote):
        self.i += 3
        if self.peek(2) == "\r\n":
            self.i += 2
        elif self.peek() == "\n":
            self.i += 1
        out = []
        while True:
            if self.i >= len(self.s):
                self.fail("unterminated multi-line string")
            if self.peek(3) == quote:
                run = 3
                while self.s[self.i + run:self.i + run + 1] == quote[0] and run < 5:   # up to two quotes end the body
                    run += 1
                out.append(quote[0] * (run - 3))
                self.i += run
                return "".join(out)
            c = self.peek()
            self.i += 1
            if quote == '"""' and c == "\\":
                if re.match(r"[ \t]*\r?\n", self.s[self.i:]):   # a line-ending backslash trims the whitespace after it
                    while self.peek() in (" ", "\t", "\n", "\r") and self.peek():
                        self.i += 1
                    continue
                out.append(self.escape())
            else:
                out.append(c)

    def array(self):
        self.i += 1
        out = []
        while True:
            self.skip_blank()
            if self.peek() == "]":
                self.i += 1
                return out
            out.append(self.value())
            self.skip_blank()
            if self.peek() == ",":
                self.i += 1
            elif self.peek() != "]":
                self.fail("expected ',' or ']' in an array")

    def inline_table(self):
        self.i += 1
        out = {}
        self.skip_ws()
        if self.peek() == "}":
            self.i += 1
            return out
        while True:
            self.pair(out)
            self.skip_ws()
            if self.peek() == ",":
                self.i += 1
            elif self.peek() == "}":
                self.i += 1
                return out
            else:
                self.fail("expected ',' or '}' in an inline table")
