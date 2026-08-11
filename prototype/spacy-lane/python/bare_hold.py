"""Bare-interpreter memory baseline: how much of the sidecar's RSS is Python itself."""

import os
import sys
import time

print(os.getpid(), flush=True)
if "--spacy" in sys.argv:
    import spacy

    nlp = spacy.load("en_core_web_sm", exclude=["ner"])
    nlp("warmup sentence for the parser")
    print("loaded", flush=True)
time.sleep(30)
