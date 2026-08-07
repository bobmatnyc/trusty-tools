Added
- `palace_unalias`: free drawers whose vector was destroyed by an id collision, so a `palace_reembed` run can make them findable again ([#5005](https://github.com/bobmatnyc/trusty-tools/issues/5005))
  - dry-run by default, like `palace_reembed`. It reports the drawer id SET (`freed_ids`), never a bare count — a count-based all-clear is the defect #5005 is about
  - callers branch on `outcome` (`clean` | `planned` | `repaired` | `partial` | `unavailable`) or `success`; `partial` and `unavailable` carry ids and neither is a success. `reembed_required` says outright when a `palace_reembed` run is still owed
  - reports `unnameable_keys`: keys in a collision group that name no drawer, so `aliased_before_ids` can be empty over a real collision. Branch on `outcome`, never on the id counts
