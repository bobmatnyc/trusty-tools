Added
- After the boot grace window, the daemon publishes the parked-sessions notice once per start as a control-bus `Hook{kind:"resume_parked"}` event carrying `sessions` (`id`, `tmux_name`, `reason`), `count` and the notice text; nothing is published when no session is parked (#10362).
