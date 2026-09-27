Fixed
- The pm-guard credential-scan test for wide bracket runs no longer times the scan against a 1 s wall clock, which failed debug CI runs at 1.00-1.17 s. It now pins the scan's work units per input size, which fixes the pass count, the charge per KiB and linear growth ([#8765](https://github.com/bobmatnyc/trusty-tools/issues/8765))
