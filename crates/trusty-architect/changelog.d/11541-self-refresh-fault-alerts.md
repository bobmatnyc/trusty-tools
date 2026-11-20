Fixed
- `poller::self_refresh`: a missing window or percent setting, a zero window, unreadable self-ctx tokens, a failed capture or a failed `self-clear.sh` start is now a `self_refresh` alert (one fingerprint per kind) in the cycle's failing set, due a push like any failure; the Architect pane's own capture and self-ctx read failures raise it too (#11541 review, #9779).
