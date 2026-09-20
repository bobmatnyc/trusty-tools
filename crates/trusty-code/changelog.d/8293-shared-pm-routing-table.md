Changed

- The delegate-mode PM card's routing table and pipeline sentence are now
  rendered from the shared `trusty_agents_common::pm_routing` rows, the same
  source trusty-mpm's PM instructions use. `assets::pm_card()` is the rendered
  card every consumer reads; a drift test fails if it stops matching the shared
  rows (#8293).
