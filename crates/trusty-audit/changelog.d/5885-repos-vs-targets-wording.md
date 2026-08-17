Fixed

`trusty-audit repos` no longer reads as a failed registration. It lists the companion `manifest.toml`, which `tga audit` writes once a sweep completes, so it is empty however many targets are registered — and its old empty-list message ("No repositories configured yet — run the guided flow to pick them") sent an operator who had just registered several repositories back to register them again. It now names `trusty-audit targets`, which answers the question that was actually asked. The README's verb table gains `add`, `targets` and `remove`, and states the distinction.
