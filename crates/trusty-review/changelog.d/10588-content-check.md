Added
- `trusty-review content check` reads every file this binary needs from the installed content and exits 1 naming each missing or empty one, with the remedy; `tctl install` and `tctl upgrade` run it after `tm content update`. Public API: `runtime_content::REQUIRED_FILES` and `runtime_content::check_required` (#10588).
