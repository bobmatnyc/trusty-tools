import os
from pathlib import Path
import subprocess
import time
root=Path(__file__).resolve().parent
py=str(root/'venv/bin/python')
path=root/'replays/selected-seek-tuning/directed_seek.json'
deadline=time.monotonic()+900
while not path.exists():
    if time.monotonic()>deadline: raise TimeoutError('selected seek tuning')
    time.sleep(2)
pid=int((path.parent/'pid').read_text())
while True:
    try: os.kill(pid,0)
    except ProcessLookupError: break
    if time.monotonic()>deadline: raise TimeoutError('selected seek stop')
    time.sleep(1)
for source,name in [('c0w100-fullkg-tuning','baseline-name-tuning'),('selected-fullkg-tuning','selected-name-tuning')]:
    subprocess.run([py,'replay.py','--run',source,'--name',name,'--split','tuning','--variants','directed_name','--query-type','kg'],cwd=root,check=True)
