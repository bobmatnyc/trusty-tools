//! The spike cases whose module does not exist yet: `test_broker.py` (14,
//! Phase 3) and `test_supervisor.py` (5, Phase 2). Each is ignored and fails
//! under `--include-ignored` until its phase replaces the body with the port.
//! The audit, journal and redaction halves of some broker cases already run
//! in `spike_audit.rs`, `spike_journal.rs` and `spike_redact.rs`.

macro_rules! pending {
    ($phase:literal, $($name:ident => $what:literal),+ $(,)?) => {
        $(
            #[test]
            #[ignore = $phase]
            fn $name() {
                panic!("not ported yet ({}): {}", $phase, $what);
            }
        )+
    };
}

pending!("Phase 3: broker",
    broker_click_and_type_drive_fixture => "click then type_text; readback clicks=1 text=hi",
    broker_denied_tool_never_reaches_driver => "kill_app -> denied_tool; daemon saw no call",
    broker_filtered_args_never_reach_driver => "click pid=1 -> pid_out_of_scope; daemon saw no call",
    broker_request_id_retry_is_not_rerun => "same request twice; clicks=1; last audit replayed=true",
    broker_call_without_request_id_is_refused => "call with no request_id -> request_id_required",
    broker_audit_line_per_call => "type_text then clipboard_read -> forward, done, refused lines",
    broker_audit_write_failure_refuses_call => "audit path is a dir -> audit_failed; clicks=0",
    broker_operator_restart_and_status => "restart/status/stop report driver_version then null",
    broker_socket_is_private => "socket 0600 in a 0700 dir",
    broker_bad_json_is_refused => "non-JSON line -> bad_request",
    broker_snapshot_strips_menu_bar_from_structured_and_text => "redacted_menu_bar_elements=4 over the socket",
    broker_menu_token_and_unsnapshotted_click_are_refused => "token_not_exposed twice; only get_window_state reached",
    broker_click_confirmation_by_readback_button => "tok-button -> confirmed=true, audited",
    broker_click_confirmation_by_readback_inert => "tok-inert -> confirmed=false, audited",
);

pending!("Phase 2: supervisor",
    supervisor_start_stop_restart => "starts=2 stops=1 restarts=1; stopped version is None",
    supervisor_start_is_idempotent_when_running => "two starts launch once",
    supervisor_refuses_unpinned_binary => "binary 0.53.0 -> '!= pinned'; no start",
    supervisor_stops_daemon_reporting_wrong_version => "daemon 0.24.0 -> error; stops=1",
    supervisor_daemon_env_turns_off_telemetry_update_and_gate => "three CUA_DRIVER_RS_* vars are false",
);
