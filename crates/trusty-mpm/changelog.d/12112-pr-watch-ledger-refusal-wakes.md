Fixed

- The daemon's PR watch now wakes the owning PM when the owner's ledger write is refused, for example when `state.json` is over its size bar. Before, a refusal that never cleared held the wake back on every tick, so the row stayed `verdict=pending` beside a green `emitted_key` and the PM was never woken. The refusal is still logged and counted, and the wake is delivered once (#12112).
