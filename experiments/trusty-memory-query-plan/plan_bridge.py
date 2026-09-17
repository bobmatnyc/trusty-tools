"""Reuse immutable experiment modules without replacing their globals or model adapters."""
from pathlib import Path
import sys

RELEVANCE = Path(__file__).resolve().parent.parent / 'trusty-memory-relevance'
GRAPH = RELEVANCE.parent / 'trusty-memory-prompt-enrichment'
sys.path.append(str(RELEVANCE))
sys.path.append(str(GRAPH))
