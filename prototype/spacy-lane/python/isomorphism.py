"""Is evaluation row 1 separable from row 4 by grammar alone?

Row 1 requires NO triple from "match exhaustiveness is a hard requirement here";
row 4 requires a triple from "librs is a fast parser" by re-walking past the
adjective. Both objects are DET ADJ NOUN. This prints the full parse of both,
plus the two cross-substituted sentences, so the claim "no POS/NP feature
separates them" is checkable rather than asserted.
"""

import spacy

PAIRS = [
    "match exhaustiveness is a hard requirement here",
    "librs is a fast parser",
    # Cross-substitution: swap the object NPs between the two subjects.
    "match exhaustiveness is a fast parser here",
    "librs is a hard requirement",
]

nlp = spacy.load("en_core_web_sm", exclude=["ner"])

for text in PAIRS:
    doc = nlp(text)
    chunks = list(doc.noun_chunks)
    obj = chunks[-1] if chunks else None
    print(f"{text!r}")
    if obj is not None:
        shape = " ".join(t.pos_ for t in obj)
        tags = " ".join(t.tag_ for t in obj)
        after = next((t for t in doc if t.idx >= obj.end_char), None)
        print(f"   object NP   : {obj.text!r}")
        print(f"   POS shape   : {shape}")
        print(f"   tag shape   : {tags}")
        print(f"   head        : {obj.root.text!r} ({obj.root.pos_})")
        print(f"   token after : {after.text!r} ({after.pos_})" if after else "   token after : <end>")
    print()
