Changed
- Loading instructional content now parses and validates the PM instruction package. A package this binary cannot parse, such as one from a newer content release with an unknown section, refuses every launch, resume and relaunch with an error naming `tm content update` or a tm upgrade. The legacy-assembly fallback that sent a prompt missing the manifest-authored rules is gone (#9012).
- `tm doctor --fix` with no instructional content reports the output-style repair as a failed step naming the remedy, so the summary no longer reads clean (#9012).
