"""dashboard_mask.py: mask_pane_text, the masker every tmux pane tail passes through before the dashboard shows it (#11065).

Why: a Decision will show a session's last N pane lines (#11065 phases 2-4). A pane shows whatever the session
printed, keys and tokens included, and the thin one-line scrubbers (_SECRET in dashboard_harness_base.py, _TOKENS
in dashboard_harness.py) miss PEM blocks, JWTs, URL credentials and Authorization headers.
What: mask_pane_text(text, *, max_lines, max_line_len), a pure function (no I/O, no state). In order: strip
ANSI/OSC escapes and control bytes; mask every secret-shaped span in place with "[masked:<shape>]", keeping the
text around it; keep the last max_lines lines; cut each line to max_line_len characters plus "…". Masking runs on
the whole text before the line cap, so a cap that cuts a PEM block can never expose part of it, and an armor whose
BEGIN or END fell outside the capture is masked to the edge of the text. The shapes mirror the Rust
credential_rules (the trusty-mpm daemon's bug-report scrubber, rules 1-10) plus the _TOKENS prefixes.
A marker names the shape only; it never carries any character of the secret.
Test: tests/test_dashboard_mask.py
"""
import re

# #11065: one marker form; the shape name is the only variable part.
MARK = "[masked:{}]"
TRUNC = "…"
INPUT_MAX = 128 * 1024            # characters kept from the end of the input before any regex runs

# ANSI and OSC escapes (tmux capture-pane -e): OSC to BEL or ST, CSI, DCS/SOS/PM/APC strings, 2- and 3-byte escapes.
_ESC = re.compile(
    r"\x1b\][^\x07\x1b\n]*(?:\x07|\x1b\\)?"
    r"|\x1b\[[0-?]*[ -/]*[@-~]"
    r"|\x1b[PX^_][^\x1b\n]*(?:\x1b\\)?"
    r"|\x1b[ -/]*[0-~]?")
_CTRL = re.compile(r"[\x00-\x08\x0b-\x1f\x7f-\x9f]")

# PEM-style armor: any "-----BEGIN X-----" ... "-----END X-----" block, private key, certificate or PGP block.
_ARMOR = re.compile(r"-----(BEGIN|END) [A-Z0-9 ]{1,60}-----")

# (shape, pattern, replacement) in priority order, applied after the armor pass. A replacement keeps a non-secret
# prefix group (header name, key name, URL scheme) and drops every character of the value.
_RULES = (
    # Rust rule 2: an Authorization header masks the rest of its line; a bare "Bearer <token>" masks the token.
    ("auth", re.compile(r"(?i)\b((?:proxy-)?authorization[ \t]*[:=][ \t]*)[^\n]+"), r"\g<1>" + MARK.format("auth")),
    ("bearer", re.compile(r"(?i)\b(bearer[ \t]+)[^\s\"',;]+"), r"\g<1>" + MARK.format("bearer")),
    # Rust rule 3: three base64url segments, the first a JSON header ("eyJ").
    ("jwt", re.compile(r"\beyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]*"), MARK.format("jwt")),
    # Rust rule 9: a URL's userinfo (user, colon, password, then "@") is masked; the scheme and host stay.
    ("url-credentials", re.compile(r"\b([a-zA-Z][a-zA-Z0-9+.\-]{0,31}://)[^:@\s/]{1,256}:[^@\s]{1,512}@"),
     r"\g<1>" + MARK.format("url-credentials") + "@"),
    # Rust rules 4-8 and every _TOKENS prefix, at the looser of the two length floors.
    ("gh-token", re.compile(r"\b(?:gh[pousr]_|github_pat_)[A-Za-z0-9_]{12,}"), MARK.format("gh-token")),
    ("api-key", re.compile(r"(?<![A-Za-z0-9_])sk-[A-Za-z0-9_\-]{12,}"), MARK.format("api-key")),
    ("aws-key", re.compile(r"\bAKIA[A-Za-z0-9_\-]{12,}|\bASIA[0-9A-Z]{16}\b"), MARK.format("aws-key")),
    ("google-key", re.compile(r"\bAIza[0-9A-Za-z_\-]{35}"), MARK.format("google-key")),
    ("slack-token", re.compile(r"\b(?:xox[baprse]|xapp)-[A-Za-z0-9\-]+"), MARK.format("slack-token")),
    # Rust rule 10 and _SECRET: key=value, key: value and JSON "key": "value" for a secret-named key.
    ("secret", re.compile(
        r"(?i)\b([\w.\-]{0,64}?(?:secret|token|passw(?:or)?d|pwd|api[ _\-]?key|access[_\-]?key|private[_\-]?key"
        r"|credentials?|bearer)[\w.\-]{0,64}[\"']?[ \t]*[:=][ \t]*)(\"[^\"\n]*\"|'[^'\n]*'|[^\s,;&\"']+)"),
     r"\g<1>" + MARK.format("secret")),
)

# A long standard-base64 run with upper, lower and digit: an unarmored key body line or blob. A run that starts
# with "/" is taken as a path. Hex digests carry no upper case and are kept.
_B64 = re.compile(r"(?<![A-Za-z0-9+/=])[A-Za-z0-9+/]{40,}={0,2}(?![A-Za-z0-9+/=])")
_PARTIAL_MARK = re.compile(r"\[(?:m(?:a(?:s(?:k(?:e(?:d(?::[a-z\-]*)?)?)?)?)?)?)?$")


def strip_escapes(text):
    """Text with ANSI/OSC escapes and control bytes removed; CRLF and lone CR become LF."""
    text = _ESC.sub("", text.replace("\r\n", "\n").replace("\r", "\n"))
    return _CTRL.sub("", text)


def _mask_armor(text):
    """Mask every armored block, including one whose BEGIN or END lies outside the text (#11065: a cut capture)."""
    out, pos, start = [], 0, None
    for m in _ARMOR.finditer(text):
        if m.group(1) == "BEGIN":
            if start is None:
                start = m.start()
            continue
        # An END: close the open block, or, with none open, mask from the end of the last kept span.
        first = pos if start is None else start
        out.append(text[pos:first])
        out.append(MARK.format("pem"))
        pos, start = m.end(), None
    if start is not None:
        out.append(text[pos:start])
        out.append(MARK.format("pem"))
        pos = len(text)
    out.append(text[pos:])
    return "".join(out)


def _b64(m):
    s = m.group(0)
    if s.startswith("/") or not (re.search(r"[A-Z]", s) and re.search(r"[a-z]", s) and re.search(r"[0-9]", s)):
        return s
    return MARK.format("base64")


def mask_secrets(text):
    """Every secret-shaped span replaced in place by "[masked:<shape>]"; no line cap, no escape strip."""
    text = _mask_armor(text)
    for _shape, rx, repl in _RULES:
        text = rx.sub(repl, text)
    return _B64.sub(_b64, text)


def _cut(line, n):
    if len(line) <= n:
        return line
    # A cut inside a marker drops the marker fragment (it holds no secret, but would read as a broken marker).
    return _PARTIAL_MARK.sub("", line[:n]) + TRUNC


def mask_pane_text(text, *, max_lines, max_line_len):
    """The last max_lines lines of pane text, escapes stripped, secrets masked, each line cut to max_line_len.

    Why: the one gate pane text passes before the dashboard serves it (#11065).
    What: strip_escapes, then mask_secrets on the whole text, then the line cap, then the length cap (a cut line
    is max_line_len characters plus "…"). max_lines 0 gives "". Input past INPUT_MAX characters keeps its tail.
    Raises ValueError for a negative max_lines or a max_line_len below 1, TypeError for a non-int cap.
    Test: tests/test_dashboard_mask.py
    """
    for name, v in (("max_lines", max_lines), ("max_line_len", max_line_len)):
        if not isinstance(v, int) or isinstance(v, bool):
            raise TypeError(f"{name} must be an int")
    if max_lines < 0 or max_line_len < 1:
        raise ValueError("max_lines must be >= 0 and max_line_len >= 1")
    if max_lines == 0 or not text:
        return ""
    text = str(text)
    if len(text) > INPUT_MAX:
        # A front cut can split a token; its first, partial line is dropped so no unprefixed tail survives.
        text = text[-INPUT_MAX:].partition("\n")[2]
    lines = mask_secrets(strip_escapes(text)).split("\n")
    return "\n".join(_cut(ln, max_line_len) for ln in lines[-max_lines:])
