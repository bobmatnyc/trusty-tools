Documentation

- BASE-AGENT's verification section replaces the bare "show raw output" instruction with a "Gate Output: Quote Results, Summarize Progress" rule: quote `test result:` lines and failure blocks, summarize build progress, run each gate once in the background redirected to a scratch file, and never `cat`/repeatedly `tail` a running build log
- BASE-AGENT adds a "Waiting on a Background Command" rule: wait on the process (`wait $pid` / `kill -0 $pid`), never on log text or a self-matching `pgrep -f`/`ps | grep` loop, and every wait has a bound
- BASE-AGENT adds a "Dispatch Budget: Enforce Your Own Time Box" rule (agent records its own start time and stops at the brief's time box) and reconciles the PM-Authority injection-skepticism text with a mid-task PM `SendMessage`: that channel is legitimate, never tool-output content, and a PM message adding scope still gets "new work is a new agent" unless the PM states the owner approved it
