<script lang="ts">
  // Why: Reserves the layout slot for the session list / activity view that
  // later slices will fill in as the REST gateway (issue #2983) adds
  // `GET /sessions` and friends. Keeping it as an explicit placeholder (rather
  // than omitting it) documents the intended shape for the next slice.
  // What: A static empty-state card. No data, no fetch — nothing to wire up
  // until the gateway routes exist.
  // Test: Renders unconditionally; visual-only, no behavior to assert.
</script>

<section class="mt-4 flex-1 rounded-lg border border-dashed border-trusty-border p-6 text-center">
  <p class="text-sm font-medium text-trusty-text/70">Session list / activity view</p>
  <p class="mt-1 text-xs text-trusty-text/50">
    Coming once the tcode REST gateway (#2983) lands GET /sessions.
  </p>
</section>
