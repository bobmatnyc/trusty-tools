Fixed

- The BM25 supervisor waits 5s after SIGTERM before escalating to SIGKILL,
  up from 2s. The daemon allows itself 2s for its shutdown snapshot flush and
  needs signal delivery, socket cleanup and exit on top, so an equal budget let
  the SIGKILL land inside the flush and lose the open write window.
- Corrected two doc-comment test pointers that named tests which did not exist,
  which failed `scripts/check_test_pointers.sh`. The restart path they pointed at
  now has a real test.
