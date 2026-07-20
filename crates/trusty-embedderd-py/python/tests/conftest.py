"""pytest configuration for the sidecar test suite.

Registers the ``real_model`` marker so the torch-free conformance suite runs
clean (no unknown-marker warnings) without adding pytest as a runtime
dependency in pyproject/uv.lock.
"""


def pytest_configure(config):
    config.addinivalue_line(
        "markers",
        "real_model: correctness test that loads the real model (needs torch + "
        "TRUSTY_RUN_REAL_MODEL=1); skipped otherwise.",
    )
