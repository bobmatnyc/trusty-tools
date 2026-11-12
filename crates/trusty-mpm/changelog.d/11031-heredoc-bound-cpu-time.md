Fixed

- The #9720 dense here-document tests for `tm hook --egress-guard` and `tm hook --pm-guard` no longer fail on a loaded CI runner. They bound the hook's CPU time (60 s), not its wall time (40 s), and a new egress test checks that doubling a line of `<<` operators at most triples its CPU time. A hook that hangs is still killed and fails the test. The guards are unchanged (#11031).
