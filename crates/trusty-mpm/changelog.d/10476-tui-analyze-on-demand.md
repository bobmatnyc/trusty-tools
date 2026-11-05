Fixed
- `tm tui` home screen: an installed trusty-analyze with no running server now shows ON DEMAND instead of DOWN, because trusty-analyze runs on demand and has no resident daemon. It still shows up when its health check answers, degraded when the answer is not health, and not installed when the binary is missing (#10476).
