Fixed

- A diversion from a parent session running on `claude-fable-5-1` now records a savings row instead of declining one. The producer prices at the parent model's input rate and writes nothing for a model the shared table cannot price; that table had no Fable/Mythos entry, so every diversion from the tier wrote nothing. The fix is in `trusty-common`'s pricing table — this crate gains the regression test that drives the real table rather than a stand-in (#6972).
