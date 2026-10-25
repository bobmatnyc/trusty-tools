Changed
- The post-upsert re-check no longer searches again for a drawer already recorded for the exact scan; it still walks on through it, so a drawer behind it is still found. Seeding time is not restored by this change ([#10204](https://github.com/bobmatnyc/trusty-tools/issues/10204))
