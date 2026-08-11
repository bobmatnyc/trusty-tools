"""Does an out-of-vocabulary object noun survive the adjective gate?

Why: the gate rejects an is-a object whose NP head is tagged ADJ. Crate and tool
names are out of vocabulary, and spaCy already tagged `rustc` as ADJ on the
subject side — so the question is whether the same mistag on the OBJECT side
silently deletes real triples. That is the false-reject failure mode the brief
called out as the one that matters most.
What: prints the NP head and its POS for is-a sentences whose object is an
invented or OOV name, plus `is_oov` for each, so the report can state how often
the gate would drop a real entity.
"""

import spacy

CASES = [
    "the storage layer is a redb",
    "the index is a tantivy",
    "the runtime is a tokio",
    "the serializer is a serde",
    "the parser is a nom",
    "the allocator is a mimalloc",
    "the format is a msgpack",
    "the transport is a quic",
    "the encoder is a zstd",
    "the shell is a zsh",
    "the daemon is a trusty-memory",
    "the cache is a moka",
]

nlp = spacy.load("en_core_web_sm", exclude=["ner"])

rejected = 0
for text in CASES:
    doc = nlp(text)
    obj = list(doc.noun_chunks)[-1]
    drop = obj.root.pos_ == "ADJ"
    rejected += drop
    print(
        f"{text:38} head={obj.root.text!r:16} pos={obj.root.pos_:6} "
        f"oov={obj.root.is_oov} {'<-- FALSE REJECT' if drop else ''}"
    )
print(f"\nfalse rejects: {rejected}/{len(CASES)}")
