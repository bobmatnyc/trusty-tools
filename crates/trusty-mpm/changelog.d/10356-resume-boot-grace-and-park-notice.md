Added
- Auto-resume boot grace: for 600 seconds after the daemon starts, a runtime exit does not count toward the flap breaker, so a cold-start storm can no longer park the fleet. Set `TRUSTY_MPM_RESUME_BOOT_GRACE_SECS` to change it; `0` turns it off. A session that keeps flapping after the window is parked as before (#10356).
- When the boot grace window ends, the daemon logs one `warn` notice (target `trusty_mpm::resume_park`) naming every session the breaker still holds parked, with the command that clears a park. It fires at most once per daemon start (#10356).
