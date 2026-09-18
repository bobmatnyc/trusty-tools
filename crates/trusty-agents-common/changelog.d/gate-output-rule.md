Documentation

- BASE-AGENT's verification section replaces the bare "show raw output" instruction with a "Gate Output: Quote Results, Summarize Progress" rule: quote `test result:` lines and failure blocks, summarize build progress, run each gate once in the background redirected to a scratch file, and never `cat`/repeatedly `tail` a running build log
