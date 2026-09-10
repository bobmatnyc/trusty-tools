Fixed

- The Stop-hook parking detector and the `tm hook --pm-guard` cost evaluator now screen the hook payload's `transcript_path` for containment under the Claude config directory before reading it, closing the two call sites #7250 left unscreened. A refused path is never opened, and the cost evaluator reports the refusal alongside its fail-open verdict instead of allowing on a 0-token reading it never took.
- Folded in #7290: the containment boundary resolves through `FrameworkPaths::under(dirs::home_dir()?)`, never `FrameworkPaths::default()` or `home_base()`, whose `"."` fallback would silently re-scope containment to the process's working directory.
