Added

- `page_read` and `page_find` read cross-origin child iframes, read-only, when the extension holds host permission for the child's own origin and the tab is in the grant's scope. Results add a `frames` table and `denied_frames` (with a reason code), every entry names its `frame`, child refs are frame-scoped (`<frame>:<epoch>.<n>`) and answer `StaleElement` once the frame navigated, and the audit line lists the child hosts read ([#11301](https://github.com/bobmatnyc/trusty-tools/issues/11301))
