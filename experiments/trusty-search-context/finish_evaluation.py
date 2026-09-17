"""Freeze tuning-only routing decision, then serial held-out evaluation."""
import json
import os
from pathlib import Path
import subprocess
import time

ROOT = Path(__file__).resolve().parent
PYTHON = str(ROOT/'venv/bin/python')

def read(path): return json.loads((ROOT/path).read_text())

def main():
    path=ROOT/'replays/selected-seek-tuning/directed_seek.json'
    deadline=time.monotonic()+1200
    while not path.exists() or not (ROOT/'replays/baseline-seek-tuning-v2/directed_seek.json').exists() or not (ROOT/'replays/selected-name-tuning/directed_name.json').exists() or not (ROOT/'replays/baseline-name-tuning/directed_name.json').exists():
        if time.monotonic()>deadline: raise TimeoutError('selected routing tuning')
        time.sleep(2)
    observations=[]
    for label, normalized, directed in [
        ('baseline','replays/baseline-fullkg-tuning/normalized.json','replays/baseline-seek-tuning-v2/directed_seek.json'),
        ('selected','replays/selected-fullkg-tuning/normalized.json','replays/selected-seek-tuning/directed_seek.json')]:
        n=read(normalized); d=read(directed)
        original=read('replays/baseline-fullkg-tuning/original.json' if label=='baseline' else 'runs/selected-fullkg-tuning/results.json')
        assert n['summary']['graph']['exact_symbol']['success_at_5'] > original['summary']['graph']['exact_symbol']['success_at_5']
        paged=read('replays/baseline-directed-tuning-v2/directed.json' if label=='baseline' else 'replays/selected-fullkg-tuning/directed.json')
        before={ (r['id'],r['lane']):[h['id'] for h in r['response']['results']] for r in paged['rows']}
        for row in d['rows']:
            assert before[(row['id'],row['lane'])]==[h['id'] for h in row['response']['results']], 'Seek changed rank order'
        named=read(f'replays/{label}-name-tuning/directed_name.json')
        for row in named['rows']:
            assert before[(row['id'],row['lane'])]==[h['id'] for h in row['response']['results']], 'Name lookup changed ranks'
        def traffic(run):
            return sum(len(json.dumps(event['response']).encode()) for row in run['rows'] for event in row['response'].get('trace',[]))
        assert traffic(d)<traffic(paged), 'Seek did not improve transfer volume'
        assert traffic(named)<traffic(d), 'Name lookup did not improve transfer volume'
        passed=d['summary']['graph']['kg']['success_at_5']>n['summary']['graph']['kg']['success_at_5'] and d['summary']['graph']['all']['success_at_5']>=n['summary']['graph']['all']['success_at_5']
        observations.append({'index':label,'directed_accepted':passed,
            'normalized_top5':n['summary']['graph']['all']['success_at_5'],
            'directed_top5':d['summary']['graph']['all']['success_at_5']})
    decision={'chunk_selection':read('results/selection.json')['selected'],
        'directed_accepted':all(o['directed_accepted'] for o in observations),
        'observations':observations,'held_out_used_for_selection':False}
    (ROOT/'results/final-config.json').write_text(json.dumps(decision,indent=2))
    print(json.dumps(decision),flush=True)
    pid=int((ROOT/'replays/selected-name-tuning/pid').read_text())
    deadline=time.monotonic()+60
    while True:
        try: os.kill(pid,0)
        except ProcessLookupError: break
        if time.monotonic()>deadline: raise TimeoutError('tuning daemon did not stop')
        time.sleep(1)
    for run,name in [('c0w100-fullkg-tuning','baseline-heldout'),('selected-fullkg-tuning','selected-heldout')]:
        subprocess.run([PYTHON,'replay.py','--run',run,'--name',name,'--split','held_out','--variants','original,normalized,directed_name'],cwd=ROOT,check=True)
    subprocess.run([PYTHON,'make_report.py'],cwd=ROOT,check=True)

if __name__=='__main__': main()
