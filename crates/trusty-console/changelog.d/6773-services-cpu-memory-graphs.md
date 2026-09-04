Added
- Each Services row on the console home page now shows the service's resident
  memory beside its %CPU, and draws two graphs side by side — CPU first, memory
  second. Both are sampled on the same 1 s tick, stored in the same per-service
  ring and delivered on the same `services` SSE event, so the two bars at any
  point are the same second. The memory graph scales to that row's own peak over
  the window rather than to a fixed ceiling. The screensaver's service table
  gained the same column and graph (#6773).
