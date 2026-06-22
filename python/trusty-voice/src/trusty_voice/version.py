"""
Why: Centralises the version string so it can be queried by CLI and tooling.
What: Exposes __version__ matching pyproject.toml.
Test: ``from trusty_voice.version import __version__``; assert non-empty string.
"""

__version__ = "0.1.0"
