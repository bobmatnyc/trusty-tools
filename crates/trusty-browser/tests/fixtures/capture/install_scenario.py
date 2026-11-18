"""Build the old symlinked-layout state used by the republish parity test."""
import os
import sys
from pathlib import Path

state = Path(sys.argv[1])
a, b = "a" * 64, "b" * 64
(state / "assets" / a / "icons").mkdir(parents=True)
(state / "assets" / b).mkdir(parents=True)
os.chmod(state, 0o700)
os.chmod(state / "assets", 0o700)
(state / "assets" / a / "stale.js").write_text("old\n")
(state / "assets" / a / "worker.js").write_text("old\n")
(state / "assets" / b / "manifest.json").write_text("x\n")
(state / "extension").symlink_to(Path("assets") / a)
