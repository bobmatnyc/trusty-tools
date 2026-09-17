Added
- `ReplEvent::SplashUpdated` and `ReplApp::splash`: engine-supplied startup splash lines that replace the banner's generic `{banner_title} v{version}` identity row, so a product can state its own launch facts once. Splash rows word-wrap to the banner's right column instead of being clipped. (#8164)
- `text::elide_middle`: shortens a string to a column budget, eliding whole path components so an over-long path keeps its deepest components whole, and falling back to head-and-tail character elision otherwise. (#8164)
