//! Why: architecture-review tranche 0 (item 4) severs the
//!      `trusty-agents-local -> cto-assistant` Cargo edge. The cto-assistant
//!      cluster (`trusty-agents-common`, `tc-services`, `trusty-cto-db`) is
//!      planned to migrate directly into `trusty-agents` rather than stay a
//!      plugin wired in from this launcher, so reworking the plugin-install
//!      call here would be throwaway effort. Until that migration lands,
//!      this binary carries no CTO plugin wiring.
//! What: Thin pass-through launcher — delegates straight to
//!       `trusty_agents::run()` with no local plugin installation.
//! Test: `cargo run -p trusty-agents-local` starts the agent server with no
//!       CTO tools exposed (behavioral change — see PR notes).

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    trusty_agents::run().await
}
