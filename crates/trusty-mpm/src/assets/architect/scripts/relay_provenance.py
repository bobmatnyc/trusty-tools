#!/usr/bin/env python3
"""relay_provenance.py: refuse an owner-labelled relay that does not quote and cite the owner (#10780).

Why: the Architect relayed its own pacing inference as `User instruction (<owner>): ... two helpers`
when the owner had said only "reset was done just now", and the PM cut the owner's helper count from four
to two for six hours. The relay protocol reserves the owner labels for the owner's words, and nothing
checked that at send time.
What: `check <session> <text>` finds every `User ruling (<name>):` and `User instruction (<name>):` label
in <text>. Its parentheses hold the owner's name, then only references (a date, a time, a record id,
`via <Name>`, `superseded`, `expired`), never words of a directive. Each one must be followed by the owner's words in double quotes, then the citation
`(owner turn: <cite>)`, then nothing or another label (`Architect answer:`, `Architect note:`,
`Architect question:`, `Architect proposal:`, or an owner label). <cite> is one of:
  chat <YYYY-MM-DDTHH:MMZ>   an owner turn in the Architect chat, read from the owner-turn record that
                             pending_hook_dedupe.py writes (inbox/owner-turns.jsonl; turns within 2 min)
  dashboard <id>             a dashboard item, Intent Ledger steer, session command or goal (bare or
                             S-qualified id); the owner-written fields of that document (an item's
                             `answer`, never the Architect's `answer_note`)
Residual risk (see #10800): the dashboard store has no field-level write authorization, so `answer`,
`cancel_note`, `wont_do.note`, `feedback.note`, `steer_requests[].text`, `il_steers`, `command` and
`goals.done_note` are trusted as owner-written without a writer check; the change-log actor check is
#10800. The owner's dashboard Other/Done/Do notes land in `answer_note`, so they are not citable; relay one
as `Architect note: ... (basis: dashboard <id>)`.
A `palace ...` cite refuses (round 2): the Architect writes every palace drawer and trusty-memory marks no
owner author, so a drawer can hold an Architect inference. A ruling known only from the palace goes out as
`Architect note: ... (basis: palace <id>)`.
The text is NFKC-normalised first. Any owner claim, `user|owner` then `ruling|instruction|decision` in any
case, that is not exactly one of the two labels refuses, and so does unlabelled text between the relay
marker and the first owner label. Each quote (split on an ellipsis) must appear in the cited turn, case and whitespace aside,
on word boundaries and with its fragments in order. #10960 (DOC-87 G-6 X17-X19, X22): no span an ellipsis elides may
hold a negation (not, no, never, n't, without, cannot); a quote under 3 words and under 12 characters must be the
whole turn; the owner-turn record is read under its shared lock (pending_hook_dedupe.owner_turns_lock), and a lock
timeout, a read error or a record ending mid-line refuses. A quote that
is in the turn but does not say the relayed thing cannot pass: the owner label carries only the quote,
and the Architect's reading of it goes after `Architect answer:` with its basis. Text with no owner
label passes untouched; so does a quoted owner ruling relayed as history once it is cited.
#10960 (DOC-87 G-4 X10-X13): a verified quote whose owner turn is a ledger owner ruling marked `superseded_by`
(`datg.py update --supersedes`) refuses and names the successor; it passes with a FLAG on stderr (X11) only when
every successor on the chain is bounded and the last `until` is still ahead. A label whose parentheses say
`superseded` relays history and is not refused. The ledgers are the Architect IL (ARCHITECT_IL, default local/il)
and every IL_SCHEDULE session, state.json and archive.jsonl. A missing ledger holds no link. A damaged record (an
unreadable or corrupt schedule, state.json or archive line) refuses only a cite whose chain touches it (see
touches); every other cite passes with a FLAG on stderr and a `finding` row in the log (Di9: no blanket gate).
Every labelled relay checked, passed or refused, is appended to inbox/relay-provenance.jsonl.
Exit 0: no owner label, or every label verified (one `cite ok` line each). Exit 3: refused (the reason,
and how to resend as an Architect answer). Exit 2: usage. Paths: ARCHITECT_OWNER_TURNS,
ARCHITECT_RELAY_LOG and DASHBOARD_ROOT override the record, the log and the dashboard store.
Test: tests/test_relay.py (through relay.sh), tests/test_relay_supersession.py
"""
import json
import os
import re
import sys
import unicodedata
from datetime import datetime, timedelta, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
if HERE not in sys.path:
    sys.path.insert(0, HERE)

LABEL = re.compile(r"User (ruling|instruction) \(([^()\n]{1,60})\):")
NEXT_LABEL = re.compile(r"(?:Architect (?:answer|note|question|proposal):|User (?:ruling|instruction) \()")
QUOTE = re.compile(r'\s*"([^"\n]+)"')
JOIN = re.compile(r"\s*(?:…|\.\.\.)?\s*(?=\")")
CITE = re.compile(r"\s*\(owner turn: ([^()\n]{1,80})\)")
TAIL = re.compile(r"[\s.;,-]*")
CHAT = re.compile(r"^chat (\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(?::\d{2})?Z)$")
DASH = re.compile(r"^dashboard ((?:S[0-9a-f]{2}:)?[A-Za-z0-9][A-Za-z0-9_.-]{0,79})$")
PALACE = re.compile(r"^palace\b", re.I)
# #10780 critic H1: an owner claim in any spelling; only the two exact labels may make one.
CLAIM = re.compile(r"\b(?:user|owner)\s+(?:ruling|instruction|decision)s?\b", re.I)
# #10780 item 2: the label's parentheses hold the owner's name (at most three capitalised words in all) and
# reference tokens only: a date or time, a record id (D97, S1a:D97, ar-T5e, #10780), `via <Name>` or a fixed
# provenance word. A lower-case word other than those is a directive or a note, and refuses.
NAME_WORD = re.compile(r"[^\W\d_]+(?:[.'-][^\W\d_]*)*")
REF_TOKEN = re.compile(r"~?\d{2}:\d[\dx](?::\d{2})?Z|\d{2}-\d{2}|\d{4}-\d{2}-\d{2}(?:T\d{2}:\d{2}(?::\d{2})?Z)?|"
                       # #10780 critic: a record id carries a digit, so a name (Ada, Dee, do-Add) is not one;
                       # only the ar-/mu- record namespaces keep digit-free ids (ar-Tfa, ar-Tbe in real labels).
                       r"(?:S(?=[0-9a-f]?\d)[0-9a-f]{2}:|[a-z]{2,4}-)?[A-Z](?=[0-9a-f]?\d)[0-9a-f]{2}|"
                       r"(?:ar|mu)-[A-Z][0-9a-f]{2}|#\d+|"
                       r"superseded|expired|dash|dashboard|chat|voice|typed|direct|verbatim|answered|relayed|"
                       r"reported|amended|re-relay")
MARKER = re.compile(r"^\[[^\]\n]*Architect [0-9]{2}:[0-9][0-9x]Z\]")
ARCH_LABEL = re.compile(r"\s*Architect (?:answer|note|question|proposal):")
ELLIPSIS = re.compile(r"…|\.\.\.")
# #10960 (DOC-87 G-6 X17): a word that flips the meaning of what an ellipsis leaves out.
# #10960: apostrophe-free contractions and negative pronouns count too.
NEGATION = re.compile(r"\b(?:not|no|never|without|cannot|dont|wont|cant|isnt|shouldnt|didnt|doesnt|nothing|none|nor|"
                      r"neither)\b|n't")
MIN_QUOTE_WORDS, MIN_QUOTE_CHARS = 3, 12   # #10960 X18: below both, the quote must be the whole turn
EDGE = " .,;:!?"
NOT_IN, NEGATED, SHORT = "not in", "negated", "short"
WINDOW = timedelta(minutes=2)
HOW = ("Quote the owner verbatim and cite the turn: User ruling (<owner>): \"<owner's words>\" "
       "(owner turn: chat <YYYY-MM-DDTHH:MMZ> | dashboard <id>). "
       "For your own reading or arithmetic send `Architect answer: ... (basis: ...)` instead.")
FOLD = str.maketrans({"“": '"', "”": '"', "‘": "'", "’": "'", "ʼ": "'"})   # #10960: U+02BC as n't


class Refused(Exception):
    """A labelled segment that is not the owner's cited words."""


def _norm(s):
    return " ".join(unicodedata.normalize("NFKC", str(s)).translate(FOLD).split()).casefold()


def owner_turns_path():
    import pending_hook_dedupe as ph   # the writer owns the record's path and lock
    return ph.owner_turns_path()


def relay_log_path():
    return os.environ.get("ARCHITECT_RELAY_LOG") or os.path.join(ROOT, "inbox", "relay-provenance.jsonl")


def _ts(s):
    for fmt in ("%Y-%m-%dT%H:%M:%SZ", "%Y-%m-%dT%H:%MZ"):
        try:
            return datetime.strptime(s, fmt).replace(tzinfo=timezone.utc)
        except (TypeError, ValueError):
            pass
    return None


def chat_turns(at):
    """The texts of the owner chat turns recorded within WINDOW of `at` (chat_records)."""
    return [text for _, text in chat_records(at)]


def chat_records(at):
    """[(at, text)] of the owner chat turns recorded within WINDOW of `at`, read under the record's shared lock.

    Refused (#10960, DOC-87 X22) when the lock is not held in time, the record cannot be read, or it ends mid-line
    (a write cut short): the check denies rather than trust a torn read. A missing record has no turns. A line that
    is not UTF-8 JSON is skipped; it cannot be a cite.
    Test: test_relay_refuses_a_torn_or_unreadable_owner_turn_record_10960,
    test_relay_refuses_while_a_writer_holds_the_owner_turn_lock_10960,
    test_relay_reads_the_owner_turn_a_locked_writer_finishes_10960
    """
    import pending_hook_dedupe as ph
    want, out, path = _ts(at), [], owner_turns_path()
    try:
        with ph.owner_turns_lock(path, shared=True):
            with open(path, "rb") as f:
                raw = f.read()
    except FileNotFoundError:
        return out
    except OSError as e:   # a lock timeout is a TimeoutError, an OSError: refuse, never read unlocked
        raise Refused(f"the owner-turn record could not be read under its lock ({e})")
    if raw and not raw.endswith(b"\n"):
        raise Refused("the owner-turn record ends mid-line (a write was cut short or the record was truncated)")
    for ln in raw.splitlines():
        try:
            rec = json.loads(ln.decode("utf-8"))
        except ValueError:   # also UnicodeDecodeError
            continue
        t = _ts(rec.get("at")) if isinstance(rec, dict) else None
        if t and want and abs(t - want) <= WINDOW and isinstance(rec.get("text"), str):
            out.append((rec["at"], rec["text"]))
    return out


def _owner_fields(coll, data):
    """The owner-written strings of one dashboard document: an item's `answer`, the notes its owner buttons
    write, and its steer texts. `answer_note` never counts: the Architect writes it ("Relayed 16:08Z", outcomes),
    and the owner's Other/Done/Do notes that also land there go out as `Architect note: ... (basis: dashboard <id>)`.
    These fields are trusted without a writer check (no field-level write authorization; see #10800).

    Test: test_relay_never_takes_the_architects_answer_note_as_owner_words_10780
    """
    out = []
    if coll == "items":
        out += [data.get("answer"), data.get("cancel_note")]   # #10780: no answer_note, whatever its prefix
        for key in ("wont_do", "feedback"):
            if isinstance(data.get(key), dict):
                out.append(data[key].get("note"))
        steers = data.get("steer_requests")
        out += [s.get("text") for s in steers if isinstance(s, dict)] if isinstance(steers, list) else []
    elif coll == "il_steers":
        out.append(data.get("text"))
    elif coll == "sessions" and isinstance(data.get("command"), dict):
        out += [data["command"].get("text"), data["command"].get("note")]
    elif coll == "goals":
        out.append(data.get("done_note"))
    return [s for s in out if isinstance(s, str) and s.strip()]


def dashboard_turns(ref):
    import dashboard_qual as dq
    import dashboard_store as ds
    store = ds.Store(os.environ.get("DASHBOARD_ROOT") or ds.DEFAULT_ROOT)
    out = []
    for coll in ("items", "il_steers", "sessions", "goals"):
        try:
            doc = store.get(coll, dq.resolve(store, coll, ref))
        except ds.StoreError:
            continue
        out += _owner_fields(coll, doc.get("data") or {})
    return out


def _fragment(p):
    """A pattern for one quote fragment that matches on word boundaries only (#10960 X18: "no" is not in "now")."""
    head = r"(?<!\w)" if re.match(r"\w", p[0]) else ""
    tail = r"(?!\w)" if re.match(r"\w", p[-1]) else ""
    return re.compile(head + re.escape(p) + tail)


def _in_order(frags, text, negation_ok=False):
    """True when the fragments occur in `text` in order, and (unless negation_ok) no span between two of them holds
    a NEGATION word (#10960 X17). Every placement is tried, so a negated first match cannot hide a clean later one."""
    pats, seen = [_fragment(p) for p in frags], {}

    def ok(i, pos):
        if i == len(pats):
            return True
        if (i, pos) not in seen:
            seen[(i, pos)] = False
            for m in pats[i].finditer(text, pos):
                if i and not negation_ok and NEGATION.search(text[pos:m.start()]):
                    break   # every later placement elides this negation too
                if ok(i + 1, m.end()):
                    seen[(i, pos)] = True
                    break
        return seen[(i, pos)]
    return ok(0, 0)


def quote_problem(quote, texts):
    """None when `quote` is the owner's words in one of `texts`; else NOT_IN, NEGATED or SHORT.

    What: the quote, normalised (_norm) and split on an ellipsis, must occur in one text in order on word boundaries
    (X18); an elided span holding a negation does not count (NEGATED, X17); a quote under MIN_QUOTE_WORDS words and
    MIN_QUOTE_CHARS characters must equal a whole text, edge punctuation aside (SHORT, X18; X19 accepts it whole).
    The length counts the quote as written, ellipsis included, so `"do ... reduce"` is long enough.
    Test: test_relay_refuses_a_quote_whose_ellipsis_drops_a_negation_10960,
    test_relay_refuses_a_short_quote_unless_it_is_the_whole_turn_10960
    """
    q = _norm(quote)
    frags = [p.strip() for p in ELLIPSIS.split(q) if p.strip(" ,;")]
    norm = [_norm(t) for t in texts]
    found = [t for t in norm if frags and _in_order(frags, t)]
    if not found:
        return NEGATED if len(frags) > 1 and any(_in_order(frags, t, True) for t in norm) else NOT_IN
    if len(q.split()) < MIN_QUOTE_WORDS and len(q) < MIN_QUOTE_CHARS and \
            not any(q.strip(EDGE) == t.strip(EDGE) for t in found):
        return SHORT
    return None


def _name_word(w):
    """A capitalised name word; each part joined by `-`, `.` or `'` is capitalised too (Jean-Luc, O'Neil)."""
    return bool(NAME_WORD.fullmatch(w)) and all(p[0].isupper() for p in re.split(r"[.'-]", w) if p)


def owner_slot_ok(slot):
    """True when a label's parentheses hold the owner's name and reference tokens only (see REF_TOKEN).

    Test: test_relay_refuses_a_directive_in_the_owner_labels_parentheses_10780,
    test_relay_sends_an_owner_label_whose_parentheses_hold_a_name_and_dates_10780
    """
    toks = [t for t in re.split(r"[,;/+\s]+", slot) if t]
    names, i = 0, 0
    while i < len(toks):
        if toks[i] == "via" and i + 1 < len(toks) and _name_word(toks[i + 1]):
            names += 1   # #10780 critic: a `via` target counts toward the three-name cap
            i += 2
            continue
        if REF_TOKEN.fullmatch(toks[i]):
            pass
        elif _name_word(toks[i]):
            names += 1
        else:
            return False
        i += 1
    return bool(toks) and names <= 3


def check_segment(seg):
    """(quotes, cite, rest) of one labelled segment; Refused when it is not quote(s) + citation."""
    quotes, pos = [], 0
    while True:
        m = QUOTE.match(seg, pos)
        if not m:
            break
        quotes.append(m.group(1))
        pos = m.end()
        j = JOIN.match(seg, pos)
        pos = j.end() if j else pos
    if not quotes:
        raise Refused("the owner label must be followed by the owner's own words in double quotes")
    c = CITE.match(seg, pos)
    if not c:
        raise Refused("the owner's quoted words carry no citation `(owner turn: ...)`")
    rest = seg[c.end():]
    lead = TAIL.match(rest).end()
    if rest[lead:] and not NEXT_LABEL.match(rest, lead):
        raise Refused("text after the citation is not the owner's; label it `Architect answer: ... (basis: ...)`")
    return quotes, c.group(1).strip(), rest


def verify(cite, quotes):
    """'chat' or 'dashboard' when every quote is in the cited turn; Refused otherwise (a palace cite always).

    What: with two or more quotes, every ordered pair (i < j) joined by ' … ' is checked as one quote too; only
    NEGATED refuses, so quotes taken from different turns (NOT_IN when joined) still pass.
    Test: test_relay_refuses_a_palace_cite_even_with_the_quoted_words_10780,
    test_relay_refuses_a_negation_elided_between_two_quotes_10960,
    test_relay_refuses_a_negated_quote_pair_beside_other_quotes_10960,
    test_relay_sends_two_quotes_taken_from_separate_owner_turns_10960
    """
    if PALACE.match(cite):   # #10780 round 2: an Architect-written drawer is never the owner's turn
        raise Refused("a palace drawer is written by the Architect, not the owner; cite `dashboard <id>` (an "
                      "owner-written answer) or a recorded `chat <YYYY-MM-DDTHH:MMZ>` owner turn, or send "
                      "`Architect note: ... (basis: palace <id>)`")
    m = CHAT.match(cite)
    if m:
        turns, kind = chat_turns(m.group(1)), "chat"
    elif DASH.match(cite):
        turns, kind = dashboard_turns(DASH.match(cite).group(1)), "dashboard"
    else:
        raise Refused(f"citation {cite!r} is not `chat <YYYY-MM-DDTHH:MMZ>` or `dashboard <id>`")
    if not turns:
        raise Refused(f"no owner turn recorded for `{cite}`")
    for q in quotes:
        why = quote_problem(q, turns)
        if why == NEGATED:   # #10960 X17
            raise Refused(f"the quote \"{q[:60]}\" leaves out a negation the owner turn `{cite}` holds between its "
                          "parts, so it says the opposite; quote the words without the ellipsis")
        if why == SHORT:   # #10960 X18
            raise Refused(f"the quote \"{q[:60]}\" is under {MIN_QUOTE_WORDS} words and {MIN_QUOTE_CHARS} "
                          f"characters and is not the whole owner turn `{cite}`; quote more of the turn")
        if why:
            raise Refused(f"the quote \"{q[:60]}\" is not in the owner turn `{cite}`")
    for i, a in enumerate(quotes):   # #10960: every pair, so an extra quote cannot break the match and hide the gap
        for b in quotes[i + 1:]:
            joined = f"{a} … {b}"
            if quote_problem(joined, turns) == NEGATED:
                raise Refused(f"the gap between the quotes \"{joined[:60]}\" leaves out a negation the owner turn "
                              f"`{cite}` holds, so together they say the opposite; quote the words between them too")
    return kind


def check(text, findings=None):
    """[(label, name, cite, kind, quotes, flag)] per owner label; Refused on the first bad one. Damage -> `findings`."""
    text = unicodedata.normalize("NFKC", text).replace("\u201c", '"').replace("\u201d", '"')   # #10780 H1
    marks = list(LABEL.finditer(text))
    starts = {m.start() for m in marks}
    for c in CLAIM.finditer(text):   # #10780 H1: an owner claim in any other spelling is not a label
        if c.start() not in starts:
            raise Refused(f"`{text[c.start():c.end() + 16].strip()}` is not an owner label; write exactly "
                          "`User ruling (<owner>):` or `User instruction (<owner>):`")
    if marks:
        m0 = MARKER.match(text)
        lead = text[m0.end() if m0 else 0:marks[0].start()]
        if lead.strip() and not ARCH_LABEL.match(lead):
            raise Refused("text before the first owner label is not the owner's; label it `Architect answer: ...`")
    out, items, damage = [], None, findings if findings is not None else []
    for i, m in enumerate(marks):
        end = marks[i + 1].start() if i + 1 < len(marks) else len(text)
        label = f"User {m.group(1)} ({m.group(2)})"
        if not owner_slot_ok(m.group(2)):   # #10780 item 2: a directive cannot ride in the parentheses
            raise Refused(f"{label}: the parentheses hold the owner's name and references only (a date, a "
                          "time, a record id, `via <Name>`, `superseded`, `expired`); put your reading after "
                          "`Architect answer:`")
        try:
            quotes, cite, _ = check_segment(text[m.end():end])
            kind, flag = verify(cite, quotes), None
            if not re.search(r"\bsuperseded\b", m.group(2)):   # #10960 G-4; a `superseded` label relays history
                if items is None:
                    items, got = ledger_items()
                    damage += got
                refuse, flag, chain, located = superseded(cite, quotes, items)
                if refuse:
                    raise Refused(flag)
                for d in damage:
                    if touches(d, cite, quotes, chain, located):
                        raise Refused(f"the ledger record {d['path']} {d['reason']} and may hold this cite's "
                                      "supersession link; repair it, or cite a ruling recorded in a sound ledger")
            out.append((label, m.group(2), cite, kind, quotes, flag))
        except Refused as e:
            raise Refused(f"{label}: {e}")
    return out


def _ledger_paths(damage):
    """state.json paths of the Architect IL and every IL_SCHEDULE session; an unreadable schedule is bytes-less damage."""
    import dashboard_il as dil
    sched = os.environ.get("IL_SCHEDULE") or os.path.join(ROOT, "local", "il-schedule.json")
    try:
        with open(sched, "rb") as f:
            json.loads(f.read().decode("utf-8"))
    except FileNotFoundError:
        pass
    except (OSError, ValueError) as e:   # also UnicodeDecodeError
        damage.append({"path": sched, "reason": f"cannot be read ({e}), so its sessions' ledgers were not read",
                       "raw": None})
    arch = os.environ.get("ARCHITECT_IL") or os.path.join(ROOT, "local", "il")
    return [r["state_path"] for r in dil.discover({}, sched, arch)]


def _read(path, damage):
    """The bytes of `path`; None when it does not exist, or when it cannot be read (recorded in `damage`)."""
    try:
        with open(path, "rb") as f:
            return f.read()
    except FileNotFoundError:
        return None
    except OSError as e:
        damage.append({"path": path, "reason": f"cannot be read ({e})", "raw": None})
        return None


def ledger_items():
    """(items, damage): every ledger item, open (state.json) and archived (archive.jsonl), and each damaged record as
    {path, reason, raw: its bytes, or None when unreadable}. A missing file holds no link and is not damage."""
    items, damage = [], []
    for sp in _ledger_paths(damage):
        raw = _read(sp, damage)
        if raw is not None:
            try:
                got = json.loads(raw.decode("utf-8")).get("items")
                if not isinstance(got, list):
                    raise ValueError("no items list")
                items += [i for i in got if isinstance(i, dict)]
            except (ValueError, AttributeError) as e:
                damage.append({"path": sp, "reason": f"is corrupt ({e})", "raw": raw})
        ap = os.path.join(os.path.dirname(sp), "archive.jsonl")
        for n, ln in enumerate((_read(ap, damage) or b"").splitlines(), 1):
            try:
                item = json.loads(ln.decode("utf-8")).get("item")
            except (ValueError, AttributeError):
                damage.append({"path": f"{ap}:{n}", "reason": "is a corrupt archive line", "raw": ln})
                continue
            if isinstance(item, dict):
                items.append(item)
    return items, damage


def touches(d, cite, quotes, chain, located):
    """True when the damaged record `d` may hold a link on this cite's chain (#10960, Di9).

    What: with its bytes, when they name the cite's turn (`chat YYYY-MM-DDTHH:MM` for any minute within 2 of the
    cited time or of a turn the quote matched; `dashboard [S..:]<id>`) or quote an id on the cite's chain. Bytes
    lost to the damage hold no link, as a missing ledger holds none. Without bytes (unreadable file or schedule):
    unless a sound record located the cite, since its links live beside it in that ledger.
    Test: test_relay_refuses_a_damaged_record_on_the_cites_chain_10960,
    test_relay_flags_a_damaged_ledger_off_the_cites_chain_and_sends_10960
    """
    if d["raw"] is None:
        return not located
    pats = [b'"' + re.escape(i.encode()) + b'"' for i in chain if isinstance(i, str)]
    m, dash = CHAT.match(cite), DASH.match(cite)
    if m:
        for t in filter(None, [_ts(m.group(1))] + [_ts(k[1]) for k in turn_keys(cite, quotes)]):
            pats += [re.escape(("chat " + (t + timedelta(minutes=k)).strftime("%Y-%m-%dT%H:%M")).encode())
                     for k in range(-2, 3)]
    elif dash:
        pats.append(rb"dashboard (?:S[0-9a-f]{2}:)?" + re.escape(dash.group(1).split(":")[-1].encode()) +
                    rb"(?![\w.-])")
    return bool(pats) and re.search(b"|".join(pats), d["raw"]) is not None


def turn_keys(cite, quotes):
    """The owner turns of `cite` that hold one of `quotes`: ('chat', at, text) or ('dashboard', id, text)."""
    m, d = CHAT.match(cite or ""), DASH.match(cite or "")
    if m:
        recs = [("chat", at, t) for at, t in chat_records(m.group(1))]
    elif d:
        recs = [("dashboard", d.group(1).split(":")[-1], t) for t in dashboard_turns(d.group(1))]
    else:
        return set()
    return {k for k in recs if any(isinstance(q, str) and quote_problem(q, [k[2]]) is None for q in quotes)}


def _src(item):
    return item.get("source") if isinstance(item.get("source"), dict) else {}


def _maybe(icite, cite):
    """Cheap pre-check: can the ledger cite `icite` name the same owner turn as `cite`?"""
    a, b = CHAT.match(icite), CHAT.match(cite)
    if a and b:
        ta, tb = _ts(a.group(1)), _ts(b.group(1))
        return bool(ta and tb) and abs(ta - tb) <= 3 * WINDOW
    a, b = DASH.match(icite), DASH.match(cite)
    return bool(a and b) and a.group(1).split(":")[-1] == b.group(1).split(":")[-1] or _norm(icite) == _norm(cite)


def _hops(old, by_id):
    """(the successors of `old` in order, the id the chain breaks at: one no ledger holds, or a loop; else None)."""
    hops, seen, cur = [], {old.get("id")}, old
    while "superseded_by" in cur:
        nxt = cur["superseded_by"]
        succ = by_id.get(nxt) if isinstance(nxt, str) else None
        if succ is None or nxt in seen:
            return hops, nxt
        hops.append(succ)
        seen.add(nxt)
        cur = succ
    return hops, None


def _verdict(old, hops, broken, cite, now):
    """(refuse, message) for a cite of the superseded ruling `old`. It only flags (DOC-87 X11) when every successor
    is bounded and the last one's `until` is still ahead; an unbounded end or a passed `until` refuses."""
    head = f"the owner turn `{cite}` is ledger ruling {old.get('id')}, superseded by "
    if broken is not None:
        return True, head + (f"{old['superseded_by']!r}; the chain breaks at {broken!r}, which no ledger holds; ask "
                             "the owner for the current ruling")
    s, last = _src(hops[0]), hops[-1]
    msg = (head + f"{hops[0].get('id')} \"{s.get('quote')}\" (owner turn: {s.get('cite')})"
           + (f"; the current ruling is {last.get('id')}" if last is not hops[0] else "") + "; cite the current ruling")
    untils = [_ts(_src(h).get("until")) for h in hops]
    if all(untils) and untils[-1] > now:
        return False, msg + f" (every successor is bounded, the last until {_src(last)['until']}; flagged, not refused)"
    return True, msg + (f" (its until {_src(last)['until']} has passed)" if untils[-1] else "")


def superseded(cite, quotes, items, now=None):
    """(refuse, message, chain, located) for an owner cite against the ledger items (#10960 G-4).

    What: an item matches when its owner turn is the cite's (turn_keys, or the cite text when the turn is gone);
    `located` says one did. A matched superseded item gives the verdict (_verdict; a refusal wins); message is None
    when none is superseded. `chain` holds the matched and successor ids, for touches.
    Test: test_relay_refuses_a_cite_of_a_superseded_owner_ruling_10960,
    test_relay_flags_a_cite_superseded_by_a_bounded_ruling_10960,
    test_relay_refuses_past_a_bounded_successor_to_an_unbounded_one_10960
    """
    now = now or datetime.now(timezone.utc)
    by_id, mine, chain, located, verdict = {i.get("id"): i for i in items}, turn_keys(cite, quotes), set(), False, None
    for it in items:
        src = _src(it)
        if not isinstance(src.get("cite"), str) or not _maybe(src["cite"], cite):
            continue
        keys = turn_keys(src["cite"], [src.get("quote")])
        if not (keys & mine if keys else _norm(src["cite"]) == _norm(cite)):
            continue
        located = True
        chain.add(it.get("id"))
        if "superseded_by" in it:
            hops, broken = _hops(it, by_id)
            chain.update([h.get("id") for h in hops] + [broken])
            v = _verdict(it, hops, broken, cite, now)
            verdict = v if verdict is None or (v[0] and not verdict[0]) else verdict
    return (verdict or (False, None)) + (chain, located)


def log(session, outcome, detail):
    rec = {"at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"), "session": session,
           "outcome": outcome, **detail}
    try:
        os.makedirs(os.path.dirname(relay_log_path()), exist_ok=True)
        with open(relay_log_path(), "a", encoding="utf-8") as f:
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")
    except OSError as e:
        print(f"relay_provenance: log not written ({e})", file=sys.stderr)


def report_damage(session, damage):
    """#10960 (Di9): each damaged ledger record is a `finding` log row and a loud FLAG on stderr, never a halt."""
    for d in damage:
        log(session, "finding", {"damaged": d["path"], "reason": d["reason"]})
        print(f"FLAG DAMAGED LEDGER: {d['path']} {d['reason']}. Owner cites whose link chain does not touch it were "
              "checked without it; repair it.", file=sys.stderr)


def main(argv):
    if len(argv) != 3 or argv[0] != "check" or not argv[1] or not argv[2]:
        print("usage: relay_provenance.py check <session> <text>", file=sys.stderr)
        return 2
    session, text, findings = argv[1], argv[2], []
    try:
        found = check(text, findings)
    except Refused as e:
        report_damage(session, findings)
        log(session, "refused", {"reason": str(e)})
        print(f"NO-OWNER-CITE: {e}. {HOW}")
        return 3
    for label, _, cite, kind, quotes, flag in found:
        log(session, "verified", {"kind": kind, "label": label, "cite": cite, "quotes": [q[:300] for q in quotes],
                                  **({"flag": flag} if flag else {})})
        print(f"cite ok: {label} {cite} ({kind})")
        if flag:   # #10960 X11: stderr, so relay.sh still sends and the caller sees it
            print(f"FLAG superseded-bounded: {label}: {flag}", file=sys.stderr)
    report_damage(session, findings)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
