Fixed
- The unlock grant takes its lock before it reads the clocks, and a stamp older than the last one seen no longer reads as a stepped-back clock, so a racing read cannot drop a live grant as a fault (#10524).
