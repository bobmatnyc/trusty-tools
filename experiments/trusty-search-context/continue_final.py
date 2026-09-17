"""Finish accepted tuning and held-out replays serially."""
import os
import subprocess
import time
from pathlib import Path
root=Path(__file__).resolve().parent
py=str(root/'venv/bin/python')
path=root/'replays/baseline-seek-tuning-v2/directed_seek.json'
deadline=time.monotonic()+900
while not path.exists():
    if time.monotonic()>deadline: raise TimeoutError('baseline fast tuning')
    time.sleep(2)
pid=int((path.parent/'pid').read_text())
while True:
    try: os.kill(pid,0)
    except ProcessLookupError: break
    if time.monotonic()>deadline: raise TimeoutError('baseline fast stop')
    time.sleep(1)
subprocess.run([py,'replay.py','--run','selected-fullkg-tuning','--name','selected-seek-tuning','--split','tuning','--variants','directed_seek'],cwd=root,check=True)
subprocess.run([py,'finish_evaluation.py'],cwd=root,check=True)
