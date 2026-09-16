Fixed

- Stack detection is memoized per project directory, so one launch pays for the nested manifest walk once instead of the five-plus times `ManifestSources::resolve` used to; a launch or an asset sync opens a fresh scope, so a changed tree is still picked up (refs [#7806](https://github.com/bobmatnyc/trusty-tools/issues/7806))
- A marker content probe reads each manifest once per detection call instead of once per needle, so three framework checks on one `package.json` no longer spend three times the shared read budget (refs [#7806](https://github.com/bobmatnyc/trusty-tools/issues/7806))
