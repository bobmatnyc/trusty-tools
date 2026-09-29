#!/usr/bin/env python3
# Full-line comment: not counted.
"""Module docstring: counted, like any string literal (#8891).

# A hash at the start of a docstring line is string text, so this counts.
"""

   # Indented full-line comment: not counted.
import os  # trailing comment after code: counted


def f():
    """One-line docstring: counted."""
    s = "a # inside a double-quoted string is not a comment"
    t = 'it\'s a # inside an escaped single-quoted string'
    return s + t  # counted


TEMPLATE = '''
# markdown heading inside a data string: counted

'''
URL = "http://example.invalid/#frag"
# The next line is a string whose text is three quotes; it opens nothing.
Q = "'''"
