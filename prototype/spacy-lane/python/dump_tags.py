"""One-shot linguistic dump for the #5399 bake-off evaluation set.

Why: before any Rust gate is written, the report owes an honest answer to
"does spaCy actually see what lane B assumes it sees" — especially the claim
that the NP chunker natively stops "an ancestor of origin main" at `ancestor`.
What: prints, per evaluation sentence, the token/POS/tag/OOV stream and every
noun chunk with its head token. No policy, no pass/fail — raw observation.
"""

import spacy

CASES = [
    "match exhaustiveness is a hard requirement here",
    "confirm the squash is an ancestor of origin main",
    "rustc is a compiler",
    "librs is a fast parser",
    "trusty-memory uses redb for persistence",
    "the daemon is a member of the process group",
    "tantivy is a search library",
]

nlp = spacy.load("en_core_web_sm", exclude=["ner"])

for text in CASES:
    doc = nlp(text)
    print("=" * 78)
    print(f"INPUT: {text}")
    print("  tokens:")
    for t in doc:
        print(f"    [{t.i}] {t.text!r:22} pos={t.pos_:6} tag={t.tag_:5} oov={t.is_oov}")
    print("  noun_chunks:")
    for c in doc.noun_chunks:
        print(f"    {c.text!r:32} root={c.root.text!r} root_pos={c.root.pos_}")
