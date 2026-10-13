Fixed
- A daemon restart no longer brings back a session whose front gate denied its launch as active. When the deny's teardown failed or was interrupted and the session's pane is still live, boot reconciliation now keeps the session at its front gate, where resume refuses it and decommission can finish the close (#9850).
