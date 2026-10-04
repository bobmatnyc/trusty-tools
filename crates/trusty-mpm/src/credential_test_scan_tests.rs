//! Test-code scan: a credential test is `#[serial]` and runs inside the
//! credential sandbox (#9123).
//!
//! Why: the #9123 audit found 24 tests across five crates that cleared one
//! credential variable and let the resolver fall through to the developer's
//! real `.env.local`, `$HOME` store or ambient token, some without `#[serial]`,
//! some then `assert_eq!`-ing — printing — what came back. A new one is a
//! single test away in any crate; this scan fails CI on it.
//! What: every `.rs` file under `crates/*/src` and `crates/*/tests` is lexed
//! (comments removed, literals kept as text) with the source scan's lexer. A
//! test fn that mutates the environment AND names a credential-shaped variable
//! (or that enters `CredentialSandbox`) is a credential test. It is safe when
//! it carries the unkeyed `#[serial]` — the group the sandbox holds; a keyed
//! group does not exclude it — and reaches `CredentialSandbox::enter`, itself
//! or through a helper fn. A mutation propagates through callers too, so a
//! helper that clears a credential makes its calling test a credential test.
//! Helpers are followed within a file and across files — by a `module::name`
//! path, a `use` that names the helper or globs its module, or `use super::*`
//! from a child test file. Every other credential test must be named
//! in [`KNOWN_UNSANDBOXED`]: an unnamed one fails, and a named one that is now
//! sandboxed or gone fails until its entry is removed, so the list only
//! shrinks. Failure messages carry file and fn names only.
//! Test: `every_credential_test_is_serial_and_sandboxed`,
//! `the_credential_test_scan_judges_each_shape`,
//! `a_helper_that_mutates_a_credential_flags_its_test`,
//! `a_helper_in_another_file_flags_its_test`,
//! `the_ratchet_judges_by_test_name`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::scan_tests::lex::{Lexed, eat, is_ident, lex, matching, skip_ws};
use super::scan_tests::names_word;

/// Calls that mutate the process environment, matched as whole identifiers.
const MUTATIONS: &[&str] = &[
    "set_var",
    "remove_var",
    // #9123: folds the developer's real `.env.local` into the environment.
    "load_env_local_once",
    "EnvVarGuard",
    "with_env",
    "in_sandbox",
    "CredentialSandbox",
];

/// The sandbox type a credential test must reach.
const SANDBOX: &str = "CredentialSandbox";

/// Fragments that make an upper-case identifier a credential variable name.
const MARKERS: &[&str] = &["TOKEN", "SECRET", "PASSWORD", "API_KEY", "_KEY"];

/// `(file under crates/, the credential tests in it that are not both
/// `#[serial]` and sandboxed)`, by name. Every entry predates #9123's guard.
/// One is an issue row: `resolve_token_full_chain_coverage`, hermetic by its
/// own means (`$HOME` and the token file pinned to a temp dir, the App
/// variables unset, every assertion redacted). Many others are
/// hermetic the same way (an injected `MemoryKeyStore`, a pinned `$HOME`, a
/// keyed serial group). About half entered when helper mutations began to
/// count. Remove a name when you move its test onto the sandbox; the scan
/// fails until you do.
const KNOWN_UNSANDBOXED: &[(&str, &[&str])] = &[
    (
        "trusty-agents/src/agents/tests/mod.rs",
        &[
            "agent_config_anthropic_pin_forces_the_direct_path",
            "agent_config_pin_routes_atlascloud",
            "agent_config_rejects_a_pin_with_no_credential",
            "override_model_fails_closed_when_the_pinned_credential_vanishes",
            "override_model_repins_a_pinned_agent",
        ],
    ),
    (
        "trusty-agents/src/api/server/tests/channel_inbound.rs",
        &[
            "a_stub_channel_sends_and_reads_back_over_http",
            "an_unauthenticated_injection_is_refused",
        ],
    ),
    (
        "trusty-agents/src/api/server/tests/global_channels.rs",
        &[
            "a_configured_token_is_never_disclosed_on_the_config_probe",
            "a_credentialed_channel_write_is_admitted_and_audited",
            "a_deleted_receiving_channel_says_its_receiver_runs_until_restart",
            "a_forced_delete_audits_the_override_and_the_orphans",
            "a_global_channel_is_created_and_updated_one_at_a_time",
            "a_global_channel_is_deleted_and_an_unknown_id_is_404",
            "a_referenced_global_channel_is_not_deleted_without_force",
            "a_tokenless_daemon_refuses_every_channel_write",
            "an_unaddressable_assistant_name_is_skipped_and_a_broken_file_is_named",
            "an_unauthenticated_create_is_refused",
            "an_unauthenticated_delete_is_refused",
            "global_channels_round_trip_preserves_config_and_rejects_a_stale_revision",
            "global_channels_serve_the_dispatchable_assistant_roster",
            "the_assistant_channel_view_names_its_inert_overlays",
            "the_retired_listener_routes_are_unregistered",
        ],
    ),
    (
        "trusty-agents/src/api/server/tests/models.rs",
        &["credential_values_never_serialized"],
    ),
    (
        "trusty-agents/src/api/server/tests/relay.rs",
        &[
            "relay_accepts_honest_identity",
            "relay_accepts_with_matching_token",
            "relay_rejects_malformed_body",
            "relay_rejects_missing_token",
            "relay_rejects_non_slack_kind",
            "relay_rejects_unknown_identity",
            "relay_rejects_unknown_tier",
            "relay_rejects_when_token_unset_server_side",
            "relay_rejects_wrong_token",
        ],
    ),
    (
        "trusty-agents/src/channels/slack.rs",
        &[
            "slack_adapter_read_uses_the_credential_the_binding_names",
            "slack_adapter_send_uses_the_credential_the_binding_names",
        ],
    ),
    (
        "trusty-agents/src/channels/telegram.rs",
        &[
            "telegram_adapter_send_uses_the_credential_the_binding_names",
            "telegram_poll_token_refuses_a_credential_outside_the_family",
        ],
    ),
    (
        "trusty-agents/src/ctrl/ctrl_turn/dispatch.rs",
        &[
            "ctrl_creds_errors_when_nothing_configured",
            "ctrl_creds_falls_back_to_openrouter",
            "ctrl_creds_model_override_applied",
            "ctrl_creds_prefers_anthropic_direct_over_openrouter",
            "ctrl_turn_rest_recovers_from_a_real_local_transport_failure",
        ],
    ),
    (
        "trusty-agents/src/llm/helpers/tests.rs",
        &[
            "create_client_env_beats_store",
            "create_client_missing_everywhere_errors_clearly",
            "create_client_resolves_key_from_store_when_env_absent",
        ],
    ),
    (
        "trusty-agents/src/llm/http/tests.rs",
        &[
            // #9123 delta: `#[ignore]`d live calls; they need the real key.
            "atlascloud_live_completion_round_trips",
            "fireworks_live_completion_still_round_trips",
            "send_raw_completion_empty_endpoint_credential_falls_back_to_store",
            "send_raw_completion_fireworks_missing_key_errors_with_fireworks_name",
            "send_raw_completion_fireworks_resolves_key_from_store_when_env_absent",
            "send_raw_completion_missing_everywhere_errors_with_provider_name",
            "send_raw_completion_resolves_key_from_store_when_env_absent",
        ],
    ),
    (
        "trusty-agents/src/mcp/tests/extensions_tests.rs",
        &["auth_resolves_from_the_environment"],
    ),
    (
        "trusty-agents/src/runtime/cli_def.rs",
        &[
            "banner_fires_when_nothing_resolves",
            "banner_suppressed_when_store_configures_a_key",
        ],
    ),
    (
        "trusty-agents/src/runtime/startup.rs",
        &["a_credential_is_resolved_from_argv_then_the_environment"],
    ),
    (
        "trusty-agents/src/system_status/credentials.rs",
        &[
            "credential_status_never_leaks_a_value",
            "env_local_tier_comes_from_the_injected_source",
            "unconfigured_provider_reports_not_configured",
        ],
    ),
    (
        "trusty-agents/src/tools/mcp_tools/mod.rs",
        &["dispatch_mcp_add_strips_undeclared_env_field"],
    ),
    (
        "trusty-agents/tests/inference_shared_adapter_e2e.rs",
        &[
            "bare_claude_model_routes_through_shared_adapter_when_flag_enabled",
            "caching_active_keeps_raw_path_even_with_flag_enabled",
        ],
    ),
    (
        "trusty-agents/tests/persona_python_plugin_wiring.rs",
        &["persona_python_plugin_is_registered_by_the_dispatch_path"],
    ),
    (
        "trusty-channels/tests/client_http.rs",
        &[
            "base_client_new_resolves_env_token",
            "env_token_beats_store",
            "slack_provider_resolves_from_store",
        ],
    ),
    (
        "trusty-channels/tests/telegram_client_http.rs",
        &[
            "base_client_new_resolves_env_token",
            "env_token_beats_store",
            "telegram_provider_resolves_from_store",
        ],
    ),
    (
        "trusty-code/src/llm/client.rs",
        &[
            "live_fireworks_call",
            "live_openrouter_call",
            "live_together_call",
            "local_slug_builds_the_local_adapter_not_openrouter",
            "missing_atlascloud_key_errors_not_falls_back",
            "missing_fireworks_key_errors_not_falls_back",
            "missing_openrouter_key_errors_at_chat_time",
            "missing_together_key_errors_not_falls_back",
        ],
    ),
    (
        "trusty-code/src/task/mock_llm.rs",
        &["real_client_builds_without_openrouter_key"],
    ),
    (
        "trusty-common/src/credentials/authority.rs",
        &[
            "absent_credential_is_missing",
            "oauth_and_api_key_shapes_resolve_through_one_entry_point",
            "qualified_refs_reach_distinct_store_rows",
            "resolve_client_never_hands_back_the_string",
            "resolved_secret_does_not_render_in_debug_or_display",
        ],
    ),
    (
        "trusty-common/src/credentials/bounded_store/tests.rs",
        &[
            "an_absent_value_is_absent_not_an_error",
            "the_env_tier_answers_without_touching_the_store",
        ],
    ),
    (
        "trusty-common/src/credentials/dotenv.rs",
        &[
            "project_env_local_beats_user_env_local",
            "user_env_local_supplies_value_when_no_project_tier",
        ],
    ),
    (
        "trusty-common/src/credentials/resolver.rs",
        &[
            "absent_everywhere_is_none",
            "dotenv_loaded_value_beats_store",
            "empty_env_var_falls_through_to_store",
            "env_beats_store",
            "falls_through_to_store",
        ],
    ),
    (
        "trusty-common/src/daemon_token.rs",
        &[
            "credential_for_ignores_a_blank_override",
            "credential_for_prefers_the_env_override",
            "credential_for_withholds_from_non_loopback",
        ],
    ),
    (
        "trusty-common/src/inference/configurator/mod.rs",
        &[
            "build_unregistered_provider_errors",
            "build_uses_registered_factory",
        ],
    ),
    (
        "trusty-common/src/inference/configurator/resolver.rs",
        &[
            "bare_slug_uses_openrouter",
            "bedrock_resolves_without_key",
            "explicit_prefix_missing_key_falls_back_to_openrouter",
            "explicit_prefix_with_key_wins",
            "local_resolves_without_key",
            "no_credential_anywhere_errors",
        ],
    ),
    (
        "trusty-common/src/inference/providers/local.rs",
        &[
            "api_key_env_override_is_used",
            "factory_builds_named_adapter_with_defaults",
            "from_env_defaults_when_unset",
            "host_env_override_appends_v1_suffix",
        ],
    ),
    (
        "trusty-common/src/memory_core/dream/tests.rs",
        &[
            "consolidate_scoped_invalid_model_returns_err",
            "consolidate_scoped_no_inference_is_noop",
            "dedup_only_config_ignores_an_ambient_openrouter_key",
            "dream_cycle_semantic_consolidation_disabled_by_default_builds_nothing",
            "dream_cycle_semantic_consolidation_invalid_model_disables_once",
            "dream_cycle_semantic_consolidation_no_inference",
            "dream_cycle_semantic_consolidation_valid_local_model_not_disabled",
            "dream_semantic_no_provider_parks_without_local_fallback",
        ],
    ),
    (
        "trusty-common/src/memory_core/semantic_consolidation/mod.rs",
        &[
            "inference_available_false_without_key",
            "resolve_openrouter_api_key_falls_back_to_env",
            "resolve_provider_local_honours_the_host_override",
            "resolve_provider_local_prefix_vetoed_when_disabled",
            "resolve_provider_local_requires_explicit_prefix",
            "resolve_provider_unprefixed_model_without_key_is_unavailable",
        ],
    ),
    (
        "trusty-common/tests/config_keys_cli.rs",
        &[
            "list_reports_ambiguous_tier_when_env_and_env_local_both_present",
            "list_reports_env_tier",
            "probe_error_body_never_leaks_the_resolved_key",
            "probe_local_fails_inside_the_shared_budget",
            "probe_local_reports_reachability_not_an_aws_chain",
            "set_rejects_unknown_and_keyless_providers",
            "set_then_list_reports_store_tier_without_value",
            "test_probe_401_is_unauthorized",
            "test_probe_404_is_model_not_found",
            "test_probe_bedrock_is_unsupported",
            "test_probe_ok_against_mock",
            "test_probe_unconfigured_when_no_key",
        ],
    ),
    (
        "trusty-common/tests/inference_adapters.rs",
        &[
            "anthropic_direct_parses_native_usage_and_tool_use",
            "anthropic_direct_translates_messages_api_request",
            "atlascloud_round_trip_no_usage_directive",
            "chat_stream_degrades_when_body_not_sse",
            "chat_stream_surfaces_http_error_for_caller_retry",
            "default_factories_register_anthropic_direct",
            "default_factories_register_openai_dialect",
            "fireworks_tool_call_round_trip_no_usage_directive",
            "http_429_maps_to_retryable_api_error",
            "local_probe_passes_and_the_request_proceeds",
            "openai_direct_receives_unprefixed_model_id_on_the_wire",
            "openai_direct_sends_bare_body",
            "openrouter_cache_accounting_and_passthrough",
            "openrouter_chat_stream_yields_incremental_deltas",
            "openrouter_receives_the_full_slug_on_the_wire",
            "openrouter_sends_the_schema_as_response_format",
            "openrouter_translates_request_and_parses_response",
            "together_direct_receives_unprefixed_model_id_on_the_wire",
            "together_tool_call_round_trip_no_usage_directive",
            "unsupported_capability_is_raised_before_any_network_call",
        ],
    ),
    (
        "trusty-common/tests/inference_foundation.rs",
        &[
            "configurator_builds_and_runs_scripted_adapter",
            "configurator_unregistered_provider_alarms",
            "provider_for_two_stage_resolution",
            "resolved_provider_debug_is_redacted",
        ],
    ),
    (
        "trusty-console/src/webhook/tests.rs",
        &[
            "from_env_meters_every_targets_inbox",
            "integration_from_env_delivery_survives_a_console_restart",
            "integration_from_env_fails_closed_when_the_secret_is_unset",
        ],
    ),
    (
        "trusty-gworkspace/src/api/auth/manager.rs",
        &[
            "refresh_does_not_overwrite_a_consent_that_landed_mid_refresh",
            "refresh_errors_when_no_client_resolves_for_profile",
            "refresh_falls_back_to_global_client_when_absent",
            "refresh_keeps_a_default_change_made_mid_refresh",
            "refresh_uses_per_profile_client_when_present",
        ],
    ),
    (
        "trusty-gworkspace/src/api/auth/oauth/client_store.rs",
        &[
            "persist_and_resolve_roundtrip",
            "persist_rejects_malformed_source",
            "persist_restricts_permissions_on_unix",
            "profile_client_source_reports_global_and_per_profile",
            "resolve_errors_when_neither_source_exists",
            "resolve_falls_back_to_global_when_absent",
            "resolve_prefers_per_profile_file_over_global",
            "resolve_propagates_malformed_per_profile_file",
        ],
    ),
    (
        "trusty-mcp/tests/mcp_config.rs",
        &["claude_code_stdio_entry_matches_trusty_mpm_golden"],
    ),
    (
        "trusty-memory/src/commands/prompt_context/tests.rs",
        &["prompt_context_logs_recall_query_shape"],
    ),
    (
        "trusty-memory/src/tools/tests.rs",
        &["dispatch_palace_unalias_frees_a_real_collision_and_is_idempotent"],
    ),
    (
        "trusty-mpm/src/bin/tm/gh_identity.rs",
        &[
            "resolve_project_aware_enforces_paired_account",
            "resolve_project_aware_fails_closed_on_switch_failure",
        ],
    ),
    (
        "trusty-mpm/src/core/gh_account_enforce.rs",
        &[
            "ambient_token_override_counts_a_whitespace_token",
            "ensure_gh_account_in_dir_accepts_the_api_answer_over_a_stale_transcript",
            "ensure_gh_account_in_dir_fails_closed_on_a_blank_probe_answer",
            "ensure_gh_account_in_dir_fails_closed_when_the_api_probe_fails",
            "ensure_gh_account_in_dir_noop_when_already_active",
            "ensure_gh_account_in_dir_refuses_under_an_ambient_token",
            "ensure_gh_account_in_dir_rejects_a_transcript_the_api_contradicts",
            "ensure_gh_account_in_dir_self_heals_mismatch",
            "ensure_gh_account_in_dir_switch_failure_is_err",
        ],
    ),
    (
        "trusty-mpm/src/core/gh_identity.rs",
        &[
            "describe_redacts_token",
            "precedence_config_dir_beats_token_env",
            "precedence_token_env_beats_account",
            "resolve_token_env_absent_falls_through",
            "resolve_token_env_present",
            "token_env_binding_removes_the_config_dir",
        ],
    ),
    (
        "trusty-mpm/src/core/git_identity.rs",
        &[
            "resolve_for_config_enforced_fails_closed_on_switch_failure",
            "resolve_for_config_enforced_self_heals_mismatch",
        ],
    ),
    (
        "trusty-mpm/src/core/oauth_token.rs",
        &[
            "resolve_oauth_token_falls_back_to_stored_file",
            "resolve_oauth_token_none_when_neither_present",
            "resolve_oauth_token_prefers_env_over_stored_file",
        ],
    ),
    (
        "trusty-mpm/src/core/session_launch/tests_skill_overrides_7751.rs",
        &[
            "prepare_session_on_an_unknown_stack_writes_no_skill_overrides",
            "prepare_session_writes_stack_profile_skill_overrides_for_a_rust_project",
            "prepare_session_writes_stack_profile_skill_overrides_for_a_svelte_project",
        ],
    ),
    (
        "trusty-mpm/src/daemon/api_tests.rs",
        &["resolve_token_full_chain_coverage"],
    ),
    (
        "trusty-mpm/src/daemon/bug_report/github_tests.rs",
        &["token_resolution_from_env", "token_resolution_from_file"],
    ),
    (
        "trusty-mpm/src/runtime/claude_code_tests.rs",
        &[
            "build_inplace_resume_command_carries_oauth_token_when_available",
            "spawn_carries_the_configured_fullscreen_renderer",
            "spawn_falls_back_with_the_tmux_option_on_an_unreadable_config",
            "spawn_keeps_the_gh_token_out_of_the_typed_line",
            "spawn_publishes_session_id_via_set_environment",
            "spawn_resume_sends_oauth_token_when_available",
            "spawn_sends_env_scrub_when_binary_available",
            "spawn_sends_the_parameterized_launch_line",
        ],
    ),
    (
        "trusty-mpm/src/secret_source_tests.rs",
        &[
            "a_store_error_is_none_and_logs_the_kind",
            "a_store_timeout_is_none_and_logs_timeout",
            "an_absent_secret_is_none_and_logs_absent",
        ],
    ),
    (
        "trusty-mpm/src/telegram/tests.rs",
        &["resolve_secret_reads_the_bot_token_from_the_process_environment"],
    ),
    (
        "trusty-mpm/tests/auth_cost.rs",
        &["key_removed_even_when_present_in_parent_env"],
    ),
    (
        "trusty-mpm/tests/daemon_env_isolation.rs",
        &["a_test_daemon_env_carries_no_secret_shaped_variable"],
    ),
    (
        "trusty-mpm/tests/sandbox_latch_9121.rs",
        &[
            "resolve_secret_answers_none_with_a_bot_token_in_the_env",
            "the_activity_key_probe_is_absent_with_a_key_in_the_env",
            "the_bug_report_token_is_none_with_a_pat_in_the_env",
        ],
    ),
    (
        "trusty-mpm/tests/session_manager_mvp.rs",
        &["handler_spawn_wires_provision_and_spawn"],
    ),
    (
        "trusty-review/src/integrations/context/atlassian.rs",
        &[
            "creds_canonical_beats_product",
            "creds_missing_when_no_token",
            "creds_pat_alias_resolves_token",
            "creds_product_fallback",
            "creds_resolve_from_canonical",
        ],
    ),
    (
        "trusty-review/src/integrations/github/auth/strategy.rs",
        &[
            "cli_token_falls_back_to_gh",
            "cli_token_missing_errors",
            "cli_token_prefers_github_token",
            "cli_token_uses_gh_token_env",
        ],
    ),
    (
        "trusty-review/src/pipeline/runner_tests.rs",
        &[
            "truncation_ratio_env_invalid_falls_back",
            "truncation_ratio_env_override_applies",
        ],
    ),
    (
        "trusty-review/src/report/redact_tests.rs",
        &[
            "apply_investigation_scrubs_configured_credentials",
            "declared_metrics_file_findings_are_scrubbed",
            "enrich_scrubs_configured_credentials_from_findings",
            "investigation_credentials_never_reach_the_rendered_report",
            "report_secrets_yields_a_usable_needle_set",
        ],
    ),
];

/// One fn item: its name, its outer attributes, and its body — as written
/// (literals included, so a variable named in a string counts) and as code
/// only (literals blanked, so a call named in a string does not).
struct FnItem {
    name: String,
    attrs: Vec<String>,
    body: String,
    code: String,
}

impl FnItem {
    /// The attribute paths, `#[` `]` and any argument list stripped.
    fn attr_paths(&self) -> impl Iterator<Item = (&str, bool)> {
        self.attrs.iter().map(|a| {
            let inner = a.trim_start_matches("#[").trim_end_matches(']');
            let path = inner.split('(').next().unwrap_or("").trim();
            (path, inner.contains('('))
        })
    }

    fn is_test(&self) -> bool {
        self.attr_paths()
            .any(|(p, _)| p == "test" || p.ends_with("::test"))
    }

    /// The unkeyed `#[serial]` / `#[serial_test::serial]`.
    fn is_unkeyed_serial(&self) -> bool {
        self.attr_paths()
            .any(|(p, args)| !args && (p == "serial" || p == "serial_test::serial"))
    }
}

fn text_of(src: &[Lexed]) -> String {
    src.iter().map(|&(c, _)| c).collect()
}

/// Qualifiers that may sit between a fn's attributes and its `fn` keyword.
const QUALIFIERS: &[&str] = &["pub", "async", "unsafe", "const", "extern"];

/// Every fn item with a body in `text`, nested ones included.
fn fn_items(text: &str) -> Vec<FnItem> {
    let src = lex(text);
    let mut out = Vec::new();
    let mut attrs: Vec<String> = Vec::new();
    let mut i = 0;
    while i < src.len() {
        let (c, code) = src[i];
        if code && c.is_whitespace() {
            i += 1;
            continue;
        }
        if eat(&src, i, "#[").is_some()
            && let Some(end) = matching(&src, i + 1, '[', ']')
        {
            attrs.push(text_of(&src[i..end]));
            i = end;
            continue;
        }
        if code && is_ident(c) && !(i > 0 && src[i - 1].1 && is_ident(src[i - 1].0)) {
            let start = i;
            while src.get(i).is_some_and(|&(c, code)| code && is_ident(c)) {
                i += 1;
            }
            let word = text_of(&src[start..i]);
            if word == "fn" {
                if let Some((item, next)) = fn_item(&src, i, std::mem::take(&mut attrs)) {
                    out.push(item);
                    i = next;
                }
                continue;
            }
            if word == "pub" && eat(&src, skip_ws(&src, i), "(").is_some() {
                i = matching(&src, skip_ws(&src, i), '(', ')').unwrap_or(i);
            }
            if !QUALIFIERS.contains(&word.as_str()) {
                attrs.clear();
            }
            continue;
        }
        attrs.clear();
        i += 1;
    }
    out
}

/// The fn whose name starts after `fn` at `at`, and where scanning resumes
/// (just inside its body, so nested fns are found too).
fn fn_item(src: &[Lexed], at: usize, attrs: Vec<String>) -> Option<(FnItem, usize)> {
    let name_at = skip_ws(src, at);
    let mut j = name_at;
    while src.get(j).is_some_and(|&(c, code)| code && is_ident(c)) {
        j += 1;
    }
    let name = text_of(&src[name_at..j]);
    let open = (j..src.len()).find(|&k| src[k].1 && matches!(src[k].0, '{' | ';'))?;
    if src[open].0 == ';' {
        return None;
    }
    let close = matching(src, open, '{', '}')?;
    let body = text_of(&src[open..close]);
    let code = src[open..close]
        .iter()
        .map(|&(c, code)| if code { c } else { ' ' })
        .collect();
    Some((
        FnItem {
            name,
            attrs,
            body,
            code,
        },
        open + 1,
    ))
}

/// Whether `body` names a credential-shaped upper-case identifier.
fn names_credential(body: &str) -> bool {
    body.split(|c: char| !is_ident(c)).any(|w| {
        w.len() > 3
            && w.starts_with(|c: char| c.is_ascii_uppercase())
            && w.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            && (MARKERS.iter().any(|m| w.contains(m)) || w.ends_with("_PAT"))
    })
}

/// The names of the fns in `items` that satisfy `seed`, plus every fn that
/// calls one of them — directly or through another such fn in the same file.
/// `calls(code, name)` decides what a call is; `seed` carries the cross-file
/// half, a fn calling another file's helper.
fn closed_under_callers(
    items: &[FnItem],
    seed: impl Fn(&FnItem) -> bool,
    calls: fn(&str, &str) -> bool,
) -> BTreeSet<&str> {
    let mut set: BTreeSet<&str> = items
        .iter()
        .filter(|f| seed(f))
        .map(|f| f.name.as_str())
        .collect();
    loop {
        let more: Vec<&str> = items
            .iter()
            .filter(|f| !set.contains(f.name.as_str()))
            .filter(|f| set.iter().any(|r| calls(&f.code, r)))
            .map(|f| f.name.as_str())
            .collect();
        if more.is_empty() {
            return set;
        }
        set.extend(more);
    }
}

/// The credential tests in one file that are not both `#[serial]` and sandboxed.
fn unsandboxed_credential_tests(text: &str) -> Vec<String> {
    let files = [("k/src/a.rs".to_string(), text.to_string())];
    unsandboxed_across(&files)
        .into_values()
        .next()
        .unwrap_or_default()
}

/// A non-test fn another file can call: the crate and module a path reaches it
/// by, its name, and the file that defines it.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Export {
    krate: String,
    module: Option<String>,
    name: String,
    rel: String,
}

/// What one round of the scan hands the next: every helper, in any file, that
/// reaches the sandbox, mutates the environment, or mutates a credential.
#[derive(Default, PartialEq, Eq)]
struct Exports {
    reach: BTreeSet<Export>,
    mutates: BTreeSet<Export>,
    mutates_credential: BTreeSet<Export>,
}

impl Exports {
    /// The names of the exports in `self` that `before` lacks.
    fn names_new_since(&self, before: &Exports) -> BTreeSet<String> {
        [
            (&self.reach, &before.reach),
            (&self.mutates, &before.mutates),
            (&self.mutates_credential, &before.mutates_credential),
        ]
        .into_iter()
        .flat_map(|(now, then)| now.difference(then).map(|e| e.name.clone()))
        .collect()
    }
}

/// One scanned file.
struct Source {
    rel: String,
    /// The crate identifier (`trusty_agents`) of the crate holding the file.
    krate: String,
    /// Test-only code: a `tests/` tree, a `*_tests.rs` / `tests.rs` /
    /// `test_*.rs` file, or a file gated `#![cfg(test)]`.
    test_only: bool,
    items: Vec<FnItem>,
    /// Each `use` item up to its `;`, comments and literals removed, and
    /// whether it sits at the file's top level rather than in an inline `mod`.
    uses: Vec<(String, bool)>,
    /// The files this one declares with `#[path = "…"]`, as paths under
    /// `crates/`.
    path_children: Vec<String>,
    /// Every identifier in the file's code, so an export it never names is
    /// skipped without a per-fn search.
    words: BTreeSet<String>,
    /// The module name another file reaches this one by; see [`module_name`].
    module: Option<String>,
    /// The files a top-level `use super::…` here can name: the file that
    /// declares this one with `#[path]`, else see [`parent_files`].
    parents: Vec<String>,
}

impl Source {
    fn new(rel: &str, text: &str) -> Self {
        let code: String = lex(text)
            .iter()
            .map(|&(c, code)| if code { c } else { ' ' })
            .collect();
        let words = code
            .split(|c: char| !is_ident(c))
            .filter(|w| !w.is_empty())
            .map(str::to_string)
            .collect();
        let base = rel.rsplit('/').next().unwrap_or(rel);
        let test_only = rel.contains("/tests/")
            || rel.contains("/benches/")
            || base == "tests.rs"
            || base.ends_with("_tests.rs")
            || base.ends_with("_test.rs")
            || base.starts_with("test_")
            || code.contains("#![cfg(test)]");
        Self {
            rel: rel.to_string(),
            krate: crate_ident(rel),
            test_only,
            items: fn_items(text),
            uses: use_items(&code),
            path_children: path_children(rel, text),
            words,
            module: module_name(rel),
            parents: parent_files(rel),
        }
    }

    /// Whether `f`, a fn in this file, calls `e`, a fn in another file: by a
    /// `module::name` path into `e`'s crate, or by its bare name after a `use`
    /// that imports it.
    fn calls(&self, f: &FnItem, e: &Export, crates: &BTreeSet<String>) -> bool {
        let Some(module) = &e.module else {
            return names_word(&f.code, &e.name) && self.imports(e, crates);
        };
        let path = format!("{module}::{}", e.name);
        let qualified = f.code.match_indices(&path).any(|(at, _)| {
            let before = &f.code[..at];
            let after = &f.code[at + path.len()..];
            if before.chars().next_back().is_some_and(is_ident)
                || after.chars().next().is_some_and(is_ident)
            {
                return false;
            }
            let start = before
                .char_indices()
                .rev()
                .find(|&(_, c)| !(is_ident(c) || c == ':'))
                .map_or(0, |(i, c)| i + c.len_utf8());
            self.reaches_crate(&before[start..], e, crates)
        });
        qualified || (names_word(&f.code, &e.name) && self.imports(e, crates))
    }

    /// Whether a path that starts `prefix` (`trusty_common::a::`, `crate::`,
    /// `super::`, or nothing) lands in `e`'s crate: a named workspace crate
    /// must be `e`'s, and any other start stays in this file's crate.
    fn reaches_crate(&self, prefix: &str, e: &Export, crates: &BTreeSet<String>) -> bool {
        let first = prefix
            .trim_start_matches(':')
            .split("::")
            .next()
            .unwrap_or("");
        if crates.contains(first) {
            first == e.krate
        } else {
            self.krate == e.krate
        }
    }

    /// Whether a `use` item here brings `e` into scope, by name or by glob.
    fn imports(&self, e: &Export, crates: &BTreeSet<String>) -> bool {
        self.uses.iter().any(|(u, top)| {
            let path = u.trim_start_matches("use").trim_start();
            // An inline `mod tests`'s `super` is this same file.
            let from = e.module.as_ref().is_some_and(|m| names_word(u, m))
                || (*top && names_word(u, "super") && self.parents.contains(&e.rel));
            from && self.reaches_crate(path, e, crates)
                && (names_word(u, &e.name) || u.contains('*'))
        })
    }
}

/// The crate identifier of a path under `crates/`: its first segment with
/// `-` read as `_`.
fn crate_ident(rel: &str) -> String {
    rel.split('/').next().unwrap_or(rel).replace('-', "_")
}

/// Every `use` item in `code`, from the keyword to its `;`, and whether it
/// sits outside every brace (a file-level `use`).
fn use_items(code: &str) -> Vec<(String, bool)> {
    code.match_indices("use")
        .filter(|&(at, _)| {
            !code[..at].chars().next_back().is_some_and(is_ident)
                && code[at + 3..].starts_with(char::is_whitespace)
        })
        .filter_map(|(at, _)| {
            let before = &code[..at];
            let top = before.matches('{').count() == before.matches('}').count();
            code[at..]
                .find(';')
                .map(|end| (code[at..at + end].to_string(), top))
        })
        .collect()
}

/// The files `text` (at `rel`) declares with `#[path = "…"]`, resolved
/// against `rel`'s directory. Only an attribute that is code counts: one a
/// comment or a string quotes is skipped, and a file never maps to itself.
///
/// #9123: about twenty real `#[path]` test files quote their own attribute in
/// a doc comment; matching the raw text mapped such a file to itself, so its
/// `use super::*` imported nothing from its real parent.
fn path_children(rel: &str, text: &str) -> Vec<String> {
    let dir = rel.rsplit_once('/').map_or("", |(dir, _)| dir);
    let lexed = lex(text);
    let chars: Vec<char> = lexed.iter().map(|&(c, _)| c).collect();
    let needle: Vec<char> = "#[path".chars().collect();
    let mut out = Vec::new();
    let mut at = 0;
    while at + needle.len() <= chars.len() {
        if chars[at..at + needle.len()] != needle[..]
            || !lexed[at..at + needle.len()].iter().all(|l| l.1)
        {
            at += 1;
            continue;
        }
        let rest = &chars[at..];
        let Some(open) = rest.iter().position(|&c| c == '"') else {
            break;
        };
        let Some(len) = rest[open + 1..].iter().position(|&c| c == '"') else {
            break;
        };
        let target: String = rest[open + 1..open + 1 + len].iter().collect();
        let child = format!("{dir}/{target}");
        if child != rel {
            out.push(child);
        }
        at += open + 1 + len;
    }
    out
}

/// The module name a file under `crates/` is reached by from another file:
/// `k/src/test_env.rs` is `test_env`, `k/src/a/mod.rs` is `a`, and
/// `trusty-x/src/lib.rs` is `trusty_x`. A binary root has none.
fn module_name(rel: &str) -> Option<String> {
    let (dir, file) = rel.rsplit_once('/')?;
    match file.strip_suffix(".rs")? {
        "mod" => dir.rsplit('/').next().map(str::to_string),
        "lib" => dir
            .strip_suffix("/src")
            .map(|k| k.rsplit('/').next().unwrap_or(k).replace('-', "_")),
        "main" | "build" => None,
        stem => Some(stem.to_string()),
    }
}

/// The files that may hold the parent module of `rel` when no file declares
/// it with `#[path]`: the parent module's `mod.rs`, `<dir>.rs`, `lib.rs` or
/// `main.rs`.
fn parent_files(rel: &str) -> Vec<String> {
    let Some((dir, file)) = rel.rsplit_once('/') else {
        return Vec::new();
    };
    let stem = file.trim_end_matches(".rs");
    let mut out = Vec::new();
    // A `mod.rs` is its directory's module, so its parent is one level up.
    let parent_dir = if stem == "mod" {
        dir.rsplit_once('/').map_or("", |(up, _)| up)
    } else {
        dir
    };
    if !parent_dir.is_empty() {
        for root in ["mod", "lib", "main"] {
            out.push(format!("{parent_dir}/{root}.rs"));
        }
        out.push(format!("{parent_dir}.rs"));
    }
    out
}

/// The fns in one file that reach the sandbox, mutate the environment, and
/// mutate a credential, cross-file helpers included.
struct Verdicts<'a> {
    reach: BTreeSet<&'a str>,
    mutates: BTreeSet<&'a str>,
    mutates_credential: BTreeSet<&'a str>,
}

/// One file's verdicts for one round. `scan` judges its tests; `export` is
/// what other files may inherit, built with [`calls_strictly`] so a same-file
/// name collision (`tempfile::Builder::new()` read as a call to this file's
/// `new`) stays in this file.
struct Judged<'a> {
    scan: Verdicts<'a>,
    export: Verdicts<'a>,
}

/// Whether `code` calls a fn named `name` the way a call to a free helper or
/// to one of `Self`'s fns reads: never `.name(`, and never `Type::name` for a
/// type other than `Self`.
fn calls_strictly(code: &str, name: &str) -> bool {
    code.match_indices(name).any(|(at, _)| {
        let (before, after) = (&code[..at], &code[at + name.len()..]);
        if before.chars().next_back().is_some_and(is_ident)
            || after.chars().next().is_some_and(is_ident)
        {
            return false;
        }
        let before = before.trim_end();
        if before.ends_with('.') {
            return false;
        }
        let Some(path) = before.strip_suffix("::") else {
            return true;
        };
        let start = path
            .char_indices()
            .rev()
            .find(|&(_, c)| !is_ident(c))
            .map_or(0, |(i, c)| i + c.len_utf8());
        let segment = &path[start..];
        segment == "Self" || !segment.starts_with(|c: char| c.is_ascii_uppercase())
    })
}

/// Judge `src` given what the previous round found in every other file.
fn judge<'a, 'e>(src: &'a Source, ext: &'e Exports, crates: &BTreeSet<String>) -> Judged<'a> {
    // Only the exports this file names at all, and never its own fns.
    let relevant = |set: &'e BTreeSet<Export>| -> Vec<&'e Export> {
        set.iter()
            .filter(|e| e.rel != src.rel && src.words.contains(&e.name))
            .collect()
    };
    let calls_any = |f: &FnItem, es: &[&Export]| es.iter().any(|e| src.calls(f, e, crates));
    let (reach_ext, mutates_ext) = (relevant(&ext.reach), relevant(&ext.mutates));
    let credential_ext = relevant(&ext.mutates_credential);
    let verdicts = |calls: fn(&str, &str) -> bool| {
        let reach = closed_under_callers(
            &src.items,
            |f| names_word(&f.code, SANDBOX) || calls_any(f, &reach_ext),
            calls,
        );
        // #9123: a mutation propagates through callers the way `reach` does, so
        // a test that clears a credential through a helper is still a
        // credential test — whether the helper or the test names the variable,
        // and (#9123 delta critic) whether the helper lives in this file or
        // another.
        let mutates = closed_under_callers(
            &src.items,
            |f| MUTATIONS.iter().any(|m| names_word(&f.code, m)) || calls_any(f, &mutates_ext),
            calls,
        );
        let mutates_credential = closed_under_callers(
            &src.items,
            |f| {
                (mutates.contains(f.name.as_str()) && names_credential(&f.body))
                    || calls_any(f, &credential_ext)
            },
            calls,
        );
        Verdicts {
            reach,
            mutates,
            mutates_credential,
        }
    };
    Judged {
        scan: verdicts(names_word),
        export: verdicts(calls_strictly),
    }
}

impl Judged<'_> {
    /// The credential tests in `src` that are not both `#[serial]` and
    /// sandboxed.
    fn unsandboxed(&self, src: &Source) -> Vec<String> {
        src.items
            .iter()
            .filter(|f| f.is_test())
            .filter(|f| {
                let sandboxed = self.scan.reach.contains(f.name.as_str());
                let credential =
                    sandboxed || self.scan.mutates_credential.contains(f.name.as_str());
                credential && !(sandboxed && f.is_unkeyed_serial())
            })
            .map(|f| f.name.clone())
            .collect()
    }

    /// Add this file's non-test fns in each set to `out`.
    fn export(&self, src: &Source, out: &mut Exports) {
        let pick = |set: &BTreeSet<&str>, into: &mut BTreeSet<Export>| {
            into.extend(
                src.items
                    .iter()
                    .filter(|f| !f.is_test() && set.contains(f.name.as_str()))
                    .map(|f| Export {
                        krate: src.krate.clone(),
                        module: src.module.clone(),
                        name: f.name.clone(),
                        rel: src.rel.clone(),
                    }),
            );
        };
        pick(&self.export.reach, &mut out.reach);
        pick(&self.export.mutates, &mut out.mutates);
        pick(&self.export.mutates_credential, &mut out.mutates_credential);
    }
}

/// The unsandboxed credential tests in `files` (`(path under crates/, text)`),
/// by file, with helpers followed across files.
///
/// Why: the #9123 delta critic found tests clearing every credential through
/// `trusty-agents/src/test_env.rs` helpers, which a same-file scan never saw.
/// What: a file's helpers are judged on their own first. Every helper found
/// that way is exported; each round re-judges the files that name a new
/// export, until a round adds none. A test-only file re-exports what it gains
/// from another file, so a test helper wrapping a test helper is followed.
/// A production file exports only its own-file verdicts: a production fn's
/// reference to another module is not a call on a test's path, and following
/// one let a router builder inherit `runtime::run`'s argv-to-environment
/// writes. Paths stay in the crate they name.
/// Test: `a_helper_in_another_file_flags_its_test`.
fn unsandboxed_across(files: &[(String, String)]) -> BTreeMap<String, Vec<String>> {
    let mut sources: Vec<Source> = files.iter().map(|(rel, t)| Source::new(rel, t)).collect();
    // A `#[path]`-declared file's `super` is the file that declares it.
    let declared_by: BTreeMap<String, String> = sources
        .iter()
        .flat_map(|s| s.path_children.iter().map(|c| (c.clone(), s.rel.clone())))
        .collect();
    for src in &mut sources {
        if let Some(parent) = declared_by.get(&src.rel) {
            src.parents = vec![parent.clone()];
        }
    }
    let crates: BTreeSet<String> = sources.iter().map(|s| s.krate.clone()).collect();
    let none = Exports::default();
    let own: Vec<Judged> = sources.iter().map(|s| judge(s, &none, &crates)).collect();
    let mut judged: Vec<Judged> = sources.iter().map(|s| judge(s, &none, &crates)).collect();
    let mut used = Exports::default();
    loop {
        let mut next = Exports::default();
        for ((src, own), now) in sources.iter().zip(&own).zip(&judged) {
            if src.test_only { now } else { own }.export(src, &mut next);
        }
        if next == used {
            break;
        }
        let fresh = next.names_new_since(&used);
        for (src, slot) in sources.iter().zip(judged.iter_mut()) {
            if fresh.iter().any(|n| src.words.contains(n)) {
                *slot = judge(src, &next, &crates);
            }
        }
        used = next;
    }
    sources
        .iter()
        .zip(&judged)
        .map(|(src, j)| (src.rel.clone(), j.unsandboxed(src)))
        .filter(|(_, tests)| !tests.is_empty())
        .collect()
}

/// Every `.rs` file under `dir`, as `(path relative to crates/, text)`.
fn rust_sources(crates: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    let entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .collect::<Result<_, _>>()
        .unwrap_or_else(|e| panic!("read an entry of {}: {e}", dir.display()));
    for entry in entries {
        let path: PathBuf = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            if name != "target" && name != "node_modules" {
                rust_sources(crates, &path, out);
            }
            continue;
        }
        if path.extension().is_some_and(|e| e == "rs") {
            let rel = path
                .strip_prefix(crates)
                .expect("under crates/")
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, std::fs::read_to_string(&path).expect("read source")));
        }
    }
}

/// Why/What: see the module docs.
/// Test: this test.
#[test]
fn every_credential_test_is_serial_and_sandboxed() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ is this crate's parent")
        .to_path_buf();
    let mut sources = Vec::new();
    let roots = std::fs::read_dir(&crates).expect("read crates/");
    for krate in roots.map(|e| e.expect("crate entry").path()) {
        for sub in ["src", "tests"] {
            if krate.join(sub).is_dir() {
                rust_sources(&crates, &krate.join(sub), &mut sources);
            }
        }
    }
    assert!(
        sources
            .iter()
            .any(|(rel, _)| rel.starts_with("trusty-agents/")),
        "the scan did not reach the workspace's other crates ({} files)",
        sources.len()
    );

    let found: BTreeMap<String, BTreeSet<String>> = unsandboxed_across(&sources)
        .into_iter()
        .map(|(rel, tests)| (rel, tests.into_iter().collect()))
        .collect();
    let (unlisted, stale) = ratchet(&found, KNOWN_UNSANDBOXED);
    assert!(
        unlisted.is_empty(),
        "#9123: a credential test must be the unkeyed `#[serial]` and enter \
         `trusty_common::credentials::test_sandbox::CredentialSandbox` (itself or \
         through a helper). Not in KNOWN_UNSANDBOXED:\n  {}",
        unlisted.join("\n  ")
    );
    assert!(
        stale.is_empty(),
        "KNOWN_UNSANDBOXED lists tests that are now sandboxed or gone; remove them:\n  {}",
        stale.join("\n  ")
    );
}

/// Compare the scan's findings with the allowlist, by test name.
///
/// Why: a per-file count let a file fix one test and add a new unsandboxed
/// one in the same PR, and still pass (#9123 code-critic).
/// What: `(unlisted, stale)` — `file: test` for each finding the list does not
/// name, and for each listed name the scan no longer finds.
/// Test: `the_ratchet_judges_by_test_name`.
fn ratchet(
    found: &BTreeMap<String, BTreeSet<String>>,
    allowed: &[(&str, &[&str])],
) -> (Vec<String>, Vec<String>) {
    let allowed: BTreeMap<&str, BTreeSet<&str>> = allowed
        .iter()
        .map(|(rel, names)| (*rel, names.iter().copied().collect()))
        .collect();
    let unlisted = found
        .iter()
        .flat_map(|(rel, tests)| {
            let ok = allowed.get(rel.as_str());
            tests
                .iter()
                .filter(move |t| !ok.is_some_and(|ok| ok.contains(t.as_str())))
                .map(move |t| format!("{rel}: {t}"))
        })
        .collect();
    let stale = allowed
        .iter()
        .flat_map(|(rel, names)| {
            let seen = found.get(*rel);
            names
                .iter()
                .filter(move |n| !seen.is_some_and(|s| s.contains(**n)))
                .map(move |n| format!("{rel}: {n}"))
        })
        .collect();
    (unlisted, stale)
}

/// Why: the count-based budget passed a file that sandboxed one test and
/// added another; by name, both the swap and a stale entry fail.
/// Test: this test.
#[test]
fn the_ratchet_judges_by_test_name() {
    let found = |names: &[&str]| -> BTreeMap<String, BTreeSet<String>> {
        let set = names.iter().map(|n| (*n).to_string()).collect();
        BTreeMap::from([("c/src/a.rs".to_string(), set)])
    };
    let allowed: &[(&str, &[&str])] = &[("c/src/a.rs", &["old"])];
    // Unchanged: passes.
    assert_eq!(ratchet(&found(&["old"]), allowed), (vec![], vec![]));
    // `old` sandboxed and `new` added in the same file: both are reported.
    assert_eq!(
        ratchet(&found(&["new"]), allowed),
        (
            vec!["c/src/a.rs: new".to_string()],
            vec!["c/src/a.rs: old".to_string()]
        )
    );
}

/// Why: a scan that cannot fail proves nothing. Each fixture is one shape the
/// rule must judge: the #9123 row shapes are flagged, the fixed shapes pass,
/// and an env test that touches no credential is not a credential test.
/// Test: this test.
#[test]
fn the_credential_test_scan_judges_each_shape() {
    let flagged = |src: &str| unsandboxed_credential_tests(src);

    // Unserialized removal of a credential (the LOW rows).
    let unserialized = "#[test]\nfn t() { unsafe { std::env::remove_var(\"GITHUB_TOKEN\"); } }";
    assert_eq!(flagged(unserialized), vec!["t".to_string()]);

    // Serialized but unsandboxed (the latent rows), via a constant name.
    let serial_only =
        "#[test]\n#[serial]\nfn t() { unsafe { std::env::set_var(TOKEN_ENV_VAR, \"x\") }; }";
    assert_eq!(flagged(serial_only), vec!["t".to_string()]);

    // Sandboxed but not serialized: the sandbox mutates the environment too.
    let sandbox_only = "#[tokio::test]\nasync fn t() { let _s = CredentialSandbox::enter(); }";
    assert_eq!(flagged(sandbox_only), vec!["t".to_string()]);

    // A keyed group does not exclude the sandbox's unkeyed one.
    let keyed = "#[test]\n#[serial(creds)]\nfn t() { let _s = CredentialSandbox::enter(); }";
    assert_eq!(flagged(keyed), vec!["t".to_string()]);

    // The fixed shapes: direct, and through a same-file helper chain.
    let fixed = "#[test]\n#[serial_test::serial]\nfn t() { let _s = CredentialSandbox::enter(); }";
    assert!(flagged(fixed).is_empty());
    let helper = "fn inner() { let _s = CredentialSandbox::enter(); }\n\
                  fn with_env(kv: &[(&str, Option<&str>)]) { inner(); }\n\
                  #[test]\n#[serial]\nfn t() { with_env(&[(\"OPENROUTER_API_KEY\", None)]); }";
    assert!(flagged(helper).is_empty());

    // Not a credential test: env mutation of a plain variable, a credential
    // named in a comment only, and a non-test fn.
    let plain = "#[test]\nfn t() { unsafe { std::env::set_var(\"NO_COLOR\", \"1\") } }";
    assert!(flagged(plain).is_empty());
    let commented = "#[test]\nfn t() { // remove_var(\"GITHUB_TOKEN\")\n}";
    assert!(flagged(commented).is_empty());
    let not_test = "fn t() { unsafe { std::env::remove_var(\"GITHUB_TOKEN\") } }";
    assert!(flagged(not_test).is_empty());

    // A test inside a `mod tests` block, behind a doc comment and `pub(crate)`.
    let nested = "#[cfg(test)]\nmod tests {\n    /// doc\n    #[test]\n    pub(crate) fn t() \
                  { unsafe { std::env::remove_var(\"X_API_KEY\") } }\n}";
    assert_eq!(flagged(nested), vec!["t".to_string()]);
}

/// Why: the code-critic round on #9123 found two files whose tests cleared a
/// credential through a same-file helper and were never flagged, because only
/// the test's own body was checked for a mutation.
/// What: a helper removes `X_API_KEY`; a keyed-serial test calls it, directly
/// and through a second helper. Both callers must be flagged.
/// Test: this test.
#[test]
fn a_helper_that_mutates_a_credential_flags_its_test() {
    let direct = "fn clear() { unsafe { std::env::remove_var(\"X_API_KEY\") } }\n\
                  #[test]\n#[serial(k)]\nfn t() { clear(); }";
    assert_eq!(unsandboxed_credential_tests(direct), vec!["t".to_string()]);
    let chained = "fn clear() { unsafe { std::env::remove_var(\"X_API_KEY\") } }\n\
                   fn reset() { clear(); }\n\
                   #[test]\n#[serial(k)]\nfn t() { reset(); }";
    assert_eq!(unsandboxed_credential_tests(chained), vec!["t".to_string()]);
    // A generic setter mutates; the credential name comes from the test.
    let generic = "fn put(k: &str) { unsafe { std::env::set_var(k, \"v\") } }\n\
                   #[test]\nfn t() { put(\"GH_TOKEN\"); }";
    assert_eq!(unsandboxed_credential_tests(generic), vec!["t".to_string()]);
}

/// Why: the #9123 delta critic found tests that clear every credential through
/// `trusty-agents/src/test_env.rs`'s `clear_all_credential_env_vars` and
/// `force_env_local_loaded`; a scan that follows same-file helpers only never
/// saw them.
/// What: helpers in `k/src/test_env.rs` and `k/src/c.rs`; callers in other
/// files reach them by a `module::name` path, a named `use`, `use super::*`
/// from a `#[path]` test file, and a crate-qualified path from an integration
/// test. Each unsandboxed caller is flagged. Not flagged: a caller sandboxed
/// through another file's helper, a same-named fn in an unrelated module or
/// another crate, and a test reaching the helper only through a production fn
/// in a third file.
/// Test: this test.
#[test]
fn a_helper_in_another_file_flags_its_test() {
    let file = |rel: &str, text: &str| (rel.to_string(), text.to_string());
    let files = [
        file(
            "k/src/test_env.rs",
            "pub fn clear_all() { unsafe { std::env::remove_var(\"CLAUDE_CODE_OAUTH_TOKEN\") } }\n\
             pub fn load() { trusty_common::credentials::load_env_local_once(); }\n\
             pub fn sandbox() -> CredentialSandbox { CredentialSandbox::enter() }",
        ),
        file(
            "k/src/qualified.rs",
            "#[test]\n#[serial(k)]\nfn by_path() { crate::test_env::clear_all(); }",
        ),
        file(
            "k/src/imported.rs",
            "use crate::test_env::{clear_all, load};\n#[test]\nfn by_use() { clear_all(); }",
        ),
        file(
            "k/src/loader.rs",
            "#[test]\n#[serial]\nfn by_loader() { crate::test_env::load(); \
             let _ = std::env::var(\"OPENROUTER_API_KEY\"); }",
        ),
        file(
            "k/src/c.rs",
            "fn wipe() { unsafe { std::env::remove_var(\"X_API_KEY\") } }\n\
             #[cfg(test)]\n#[path = \"c_tests.rs\"]\nmod tests;",
        ),
        file(
            "k/src/c_tests.rs",
            "use super::*;\n#[test]\nfn by_super() { wipe(); }",
        ),
        file(
            "k/src/d.rs",
            "fn wipe() { unsafe { std::env::remove_var(\"Y_API_KEY\") } }\n\
             #[cfg(test)]\n#[path = \"d_tests.rs\"]\nmod tests;",
        ),
        // #9123: the doc comment cites the file's own `#[path]`; it is not code.
        file(
            "k/src/d_tests.rs",
            "//! Declared by `#[path = \"d_tests.rs\"]` in d.rs.\nuse super::*;\n#[test]\nfn by_cited_super() { wipe(); }",
        ),
        file(
            "k/src/fixed.rs",
            "#[test]\n#[serial]\nfn sandboxed() { let _s = crate::test_env::sandbox(); \
             crate::test_env::clear_all(); }",
        ),
        file(
            "k/src/unrelated.rs",
            "#[test]\nfn other() { other::clear_all(); }",
        ),
        file(
            "k/tests/it.rs",
            "#[test]\nfn by_crate() { k::test_env::clear_all(); }",
        ),
        file(
            "q/src/foreign.rs",
            "#[test]\nfn foreign() { crate::test_env::clear_all(); }",
        ),
        file(
            "k/src/prod.rs",
            "pub fn run() { crate::test_env::clear_all(); }",
        ),
        file(
            "k/src/via_prod.rs",
            "#[test]\nfn through_prod() { crate::prod::run(); }",
        ),
    ];
    let found = unsandboxed_across(&files);
    let flagged: Vec<(&str, &str)> = found
        .iter()
        .flat_map(|(rel, tests)| tests.iter().map(move |t| (rel.as_str(), t.as_str())))
        .collect();
    assert_eq!(
        flagged,
        vec![
            ("k/src/c_tests.rs", "by_super"),
            ("k/src/d_tests.rs", "by_cited_super"),
            ("k/src/imported.rs", "by_use"),
            ("k/src/loader.rs", "by_loader"),
            ("k/src/qualified.rs", "by_path"),
            ("k/tests/it.rs", "by_crate"),
        ]
    );
}
