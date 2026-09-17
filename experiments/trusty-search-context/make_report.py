"""Render evidence tables after selection and held-out evaluation finish."""
from __future__ import annotations
import json
from pathlib import Path
import statistics
from presentation import navigation_toc
from benchmark import size
ROOT = Path(__file__).resolve().parent

def read(path):
    return json.loads((ROOT / path).read_text())

def metrics(result, lane='graph'):
    return result['summary'][lane]['all']

def render():
    selection = read('results/selection.json')
    lines = ['# Trusty Search: chunking, BM25 and knowledge-graph experiment', '',
        '2026-09-17 · Frozen source `90b6aeb944e1d010f3690281ec24efe7b89435c3` · No embeddings', '',
        '## Findings and recommendation', '',
        'The selected combination finds the labelled source in the top five for **8/16 held-out questions**, versus **1/16** for baseline: seven gains and no losses. Context enrichment alone reaches 3/16; adding definition routing reaches 7/16; explicit graph traversal reaches 8/16. Keep the current 100-line oversized subwindows, use bounded source context, return a lean file-grouped TOC, and route clear relationship questions through the graph with ambiguity-aware fallback.', '',
        'This improves source localization and result payload size, but is not an overall latency win: held-out median query time rises from 41 to 72 ms, p95 from 335 to 1,057 ms, and sampled indexing RSS from 2.18 to 2.47 GiB. The literal lookup fallback remains expensive. All four held-out documentation labels remain unretrieved, although incomplete labels can exclude valid alternate sources. A larger set of real user queries and an indexed definition lookup are needed before making this the production default.', '',
        'The 128-word context budget won the prespecified six-cell rule; 64 words was very close on tuning. No evidence here supports shrinking oversized windows to 64 lines. Rich summary cards cost more tokens than existing compact output; the lean TOC cuts held-out payload tokens by 51–53% while preserving every locator.', '',
        '## Design', '',
        'A custom isolated daemon indexed independent copies of this repository. Six treatments crossed 0/64/128 words of deterministic source context with 100/64-line overlapping subwindows for oversized AST chunks. Existing AST boundaries and umbrella parents were retained. Context included relative path, symbol, documentation and immediate parent information; it was appended to BM25 virtual terms without changing source locations. No LLM summaries were generated. Baseline used the same custom binary with enrichment disabled and vector calls blocked; this is a controlled algorithm comparison, not a live production-daemon benchmark.', '',
        'The 48 authored, source-labelled questions (not sampled production traffic) were frozen before measurement: 32 tuning and 16 held-out, evenly divided among definitions, behavior, callers/callees and documentation. Success means the returned path and line range overlap a labelled source, not that an answer is correct. Labels are incomplete; alternate valid sources count as misses. For example, workspace MSRV guidance returned from CLAUDE.md is useful although the label names an ADR.', '',
        '## Chunk/context tuning', '',
        '| Context words | Subwindow lines | Top-5 hits / 32 | MRR@10 | Chunks | Index seconds | Compact tokens |',
        '|---:|---:|---:|---:|---:|---:|---:|']
    for t in sorted(selection['treatments'], key=lambda t:t['name']):
        r = read(f"runs/{t['name']}/results.json")
        m = r['manifest']
        lines.append(f"| {m['words']} | {m['window']} | {round(t['success5']*32)} | {t['mrr10']:.3f} | {t['chunks']:,} | {t['index_seconds']:.1f} | {t['compact_tokens']:.0f} |")
    lines += ['', f"The frozen decision rule selected **{selection['words']} context words / {selection['window']}-line subwindows**. Selection used tuning only, maximizing mean lexical/graph success@5, then MRR, compact tokens and index bytes. The two search lanes returned identical rankings in this grid. Best means best in these six treatments, not a universal optimum.", '',
        'BM25 parameters remained fixed at k1=1.5 and b=0.75. This experiment tuned indexed context, subwindow size and query routing; it did not sweep BM25 saturation/length-normalization parameters.', '', '## Held-out comparison (graph stage and adapters)', '',
        '| Index | Query behavior | Top-1 / 16 | Top-5 / 16 | Top-10 / 16 | MRR@10 | Warm p50 / p95 ms |',
        '|---|---|---:|---:|---:|---:|---:|']
    runs = {}
    for label, folder in [('Baseline','baseline-heldout'),('Selected','selected-heldout')]:
        for variant in ('original','normalized','directed_name'):
            path = ROOT / f'replays/{folder}/{variant}.json'
            if not path.exists():
                raise ValueError(f'Missing required held-out result: {path}')
            r = json.loads(path.read_text()); runs[(label,variant)] = r; a = metrics(r)
            lines.append(f"| {label} | {variant} | {round(a['success_at_1']*16)} | {round(a['success_at_5']*16)} | {round(a['success_at_10']*16)} | {a['mrr_at_10']:.3f} | {a['warm_p50_ms']:.1f} / {a['warm_p95_ms']:.1f} |")
    if not runs:
        raise ValueError('Held-out evaluation is missing')
    lines += ['', '`original` submits the untouched question. `normalized` changes only anchored “Where is IDENTIFIER defined?” to `fn IDENTIFIER`. `directed_name` adds query-derived, one-hop CallsFunction traversal for explicit calls/callers questions; all other questions use normalized search. It first resolves a unique symbol directly in the graph, falling back to at most three exact-symbol lexical seeds and ten neighbors without reading expected-answer labels. The adapter includes all search/traversal/materialization requests in latency.', '',
        '### Held-out top-five hits by question type', '',
        '| Index / query behavior | Definition / 4 | Behavior / 4 | KG / 4 | Docs / 4 |', '|---|---:|---:|---:|---:|']
    for (label,variant), result in runs.items():
        values=[round(result['summary']['graph'][kind]['success_at_5']*4) for kind in ('exact_symbol','behavior','kg','documentation')]
        lines.append(f"| {label} / {variant} | "+' | '.join(map(str,values))+' |')
    lines += ['', '### Paired top-five changes', '']
    base = {r['id']:r for r in runs[('Baseline','original')]['rows'] if r['lane']=='graph'}
    for key, result in runs.items():
        if key == ('Baseline','original'): continue
        rows = [r for r in result['rows'] if r['lane']=='graph']
        win=sum(r['quality']['success_at_5']>base[r['id']]['quality']['success_at_5'] for r in rows)
        loss=sum(r['quality']['success_at_5']<base[r['id']]['quality']['success_at_5'] for r in rows)
        lines.append(f"- {key[0]} / {key[1]} versus original baseline: {win} wins, {loss} losses, {16-win-loss} ties.")
    lines += ['', '## Knowledge graph', '',
        'The default graph stopped at 100,000 nodes. Baseline and selected treatment were separately rebuilt with `TRUSTY_MAX_KG_NODES=0` for the final comparisons; actual node and edge counts are preserved below. This avoids calling a capped graph complete. Graph extraction remains limited to relationships the current parser/resolver can establish.', '',
        '| Index | Nodes | Edges | Embedding calls | Stored vectors |', '|---|---:|---:|---:|---:|']
    for label in ('Baseline','Selected'):
        r=runs[(label,'original')]; g=r['graph_stats']
        lines.append(f"| {label} | {g['node_count']:,} | {g['edge_count']:,} | {r['final_evidence']['embedding_calls']} | {r['status']['semantic_coverage'].get('vectors_present',0)} |")
    lines += ['', 'Current graph expansion discounts neighbor scores after reciprocal-rank fusion. With one lexical lane, a typical best call neighbor scores about 0.70/61, below the tenth lexical result at 1/70; even 0.85/61 remains below 1/70. This explains why graph expansion can exist yet fail to change the visible top ten. The directed adapter tests graph traversal separately; it does not demonstrate a general solution to KG ranking.', '',
        '### KG efficiency iterations (baseline tuning, eight questions)', '',
        '| Method | Top-five / 8 | Warm median ms | Serialized backend response tokens/query |', '|---|---:|---:|---:|']
    for method, path in [('Current graph lane','runs/c0w100-fullkg-tuning/results.json'),('Directed + file pages','replays/baseline-directed-tuning-v2/directed.json'),('Directed + exact seeks','replays/baseline-seek-tuning-v2/directed_seek.json'),('Unique-name fast path','replays/baseline-name-tuning/directed_name.json')]:
        result=read(path); rows=[r for r in result['rows'] if r['lane']=='graph' and r['type']=='kg']; a=result['summary']['graph']['kg']
        transfers=[sum(size(t['response'])['tokens'] for t in r['response']['trace']) if 'trace' in r['response'] else size(r['response'])['tokens'] for r in rows]
        lines.append(f"| {method} | {round(a['success_at_5']*8)} | {a['warm_p50_ms']:.1f} | {statistics.mean(transfers):.0f} |")
    lines += ['', 'Backend payload tokens are a normalized serialization measure, not billed LLM tokens or exact HTTP wire bytes. Final result views exclude diagnostic traces. Direct traversal may expose resolver mistakes (for example a timeout method edge pointing at an unrelated type); it is evidence to inspect, not proof of semantic correctness. The fast-name path accepts only unambiguous graph symbols and falls back when unresolved. The cursor adapter assumes the frozen numeric-ending chunk-ID grammar and fails closed; a production implementation should expose a supported exact-ID batch fetch.', '',
        '## Result presentation', '',
        '| Held-out result set | Existing compact tokens | Rich cards | Lean TOC | TOC reduction vs compact |', '|---|---:|---:|---:|---:|']
    for (label,variant), r in runs.items():
        if variant != 'original': continue
        rows=[x for x in r['rows'] if x['lane']=='graph']; a=metrics(r)
        toc=statistics.mean(size(navigation_toc('experiment', x['response']['results']))['tokens'] for x in rows)
        lines.append(f"| {label} | {a['mean_compact_tokens']:.0f} | {a['mean_cards_tokens']:.0f} | {toc:.0f} | {100*(1-toc/a['mean_compact_tokens']):.1f}% |")
    lines += ['', 'The lean TOC groups paths once and retains each hit’s original rank, symbol and exact line range plus up to twelve source words. Rich cards retain longer source descriptions and declaration landmarks. These are offline canonical JSON sizes using cl100k_base, excluding common response envelopes. The TOC omits evidence, scores and explanations; its token savings do not prove equal comprehension or fewer follow-up reads. Raw source results remain available for expansion.', '',
        '## Verification and limitations', '',
        '- Accepted runs recorded zero embedding calls and zero stored vectors. A rejecting embedder makes accidental calls observable; early rejected runs are retained with failure evidence and excluded from scores.',
        '- Rust tests: 2,834 passed, 43 ignored across 36 targets; experiment example tests, clippy with warnings denied, formatting and repository SLD checks passed. Python evaluation/adapters: 65 tests passed and strict type checks passed across nine files. Review evidence is retained in results/critic.md, results/security.md and results/adapter-critic.md.',
        '- The daemon uses a debug build. Relative timings are workstation observations, not production latency promises. Some tuning indexing overlapped other experiment work; final held-out replay is serial after indexing. Three shuffled warm passes check stable rankings. First observations are not true cold-cache benchmarks.',
        '- Source corpus covers 7,231 walked files. Four tracked documentation symlinks were omitted consistently; the source checksum manifest records them. Smaller windows increase chunk count but do not redesign the AST chunker or remove umbrella overlap.',
        '- The held-out set is only 16 queries. Documentation labels omit alternate valid sources; query routing uses narrow syntax patterns. No statistically general retrieval or agent-task-completion claim is justified.',
        '- Experiment code is isolated on branch codex/search-context-experiment. Production daemon/indexes were not replaced, and nothing was merged or installed.', '',
        '## Indexing resource observations', '', '| Tuning index | Sampled peak RSS (GiB) | Logical index bytes (GiB) |', '|---|---:|---:|', *[f"| {name} | {read('runs/'+name+'/manifest.json')['peak_sampled_rss_kib']/1048576:.2f} | {read('runs/'+name+'/manifest.json')['index_bytes']/1073741824:.2f} |" for name in ('c0w100-tuning-v3',selection['selected'])], '', 'These are sampled process RSS and allocated file lengths, not exact total machine cost or live payload size. Context enrichment trades memory and indexing work for retrieval quality.', '', '## Research context', '',
        '[Anthropic’s Contextual Retrieval](https://www.anthropic.com/engineering/contextual-retrieval) adds chunk context to BM25 as well as embedding input. Its published 49% retrieval-failure reduction combines contextual embeddings and contextual BM25; it is not evidence for a BM25-only effect of that size. [cAST](https://arxiv.org/abs/2506.15655) studies AST-aware structural chunking for code retrieval. Trusty already uses AST extraction, so our subwindow grid is narrower than reproducing that algorithm.', '',
        '## Reproduce and inspect', '',
        '- Protocol: protocol/evaluation.md and protocol/amendment-01.md; frozen labels: queries.json and dataset-rationale.md.',
        '- Index one cell: `venv/bin/python run_treatment.py --words 128 --window 100 --kg-cap 0 --name NEW-NAME` (choose a new name and unused port). Source archive and the built custom daemon must already exist.',
        '- Reopen an accepted stopped fixture: `venv/bin/python replay.py --run ACCEPTED-RUN --name NEW-REPLAY --split tuning --variants original,normalized,directed_name`. Dedicated data directories, ownership checks and corpus locks are enforced.',
        '- Raw evidence: runs/*/{manifest,results}.json and replays/*/{original,normalized,directed,directed_seek,directed_name}.json, including repeated responses, provenance, graph counts and zero-vector evidence.',
        '- Rust implementation: worktree/crates/trusty-search/src/core/experiment.rs and worktree/crates/trusty-search/examples/context_experiment_daemon.rs. Evaluation adapters live beside this report’s parent directory.', '']
    (ROOT/'results/report.md').write_text('\n'.join(lines))

if __name__=='__main__': render()
