Changed
- The analyze service card reads `Available` as "installed and startable"
  rather than as a degradation: trusty-analyze runs on demand, so nothing
  listening is its correct resting state. The connector deliberately does not
  start it — the console polls detect, and a detector that started the service
  would keep it resident for as long as a dashboard tab was open (#6350).
