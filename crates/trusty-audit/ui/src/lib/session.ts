import { invoke } from '@tauri-apps/api/core';

/**
 * The one seam between this window and the auditor client's API.
 *
 * Why: DOC-68 §11 fixes the shell as a view over `Session::execute`, never a
 * second place a capability can live. Every `invoke` in this app goes through
 * this module, so "does the shell reach the backend anywhere else?" is a
 * question about one file. The functions here add no logic — each one names a
 * `session::Command` and hands back what `Session::execute` produced.
 * What: typed mirrors of the Rust DTOs in `src-tauri/src/guided.rs`, plus one
 * call per wired capability.
 * Test: exercised by launching the app; the Rust side's own shapes are proven
 * by `trusty_audit::session::session_tests`.
 */

/** One repository the engagement's manifest names. */
export interface RepositoryView {
  name: string;
  path: string;
}

/** The companion `manifest.toml`, when a previous run left one. */
export interface ManifestView {
  title: string;
  client: string | null;
  analyst: string | null;
  repositories: RepositoryView[];
}

/** One pinned tool and whether this client placed it. */
export interface ToolView {
  name: string;
  installed: boolean;
  /** Absent when the binary is missing, or present but not placed by us. */
  version: string | null;
  path: string;
}

/**
 * What the guided flow says to do next.
 *
 * The wording is this window's to choose — the Rust side names the state, not
 * the sentence, so the CLI and the shell can phrase it differently without
 * either one deriving the state itself.
 */
export type NextStepView =
  | { kind: 'select-repositories' }
  | { kind: 'install-tools'; missing: string[] }
  | { kind: 'ready-for-run' }
  | { kind: 'return-package' };

/** The whole of `Command::Guided`'s outcome. */
export interface GuidedView {
  root: string;
  manifest: ManifestView | null;
  tools: ToolView[];
  next: NextStepView;
}

/**
 * Run `Command::Guided` and hand back its outcome.
 *
 * Rejects with the `AuditError` text when the working directory cannot be
 * created or a companion file cannot be read — the window shows that string
 * rather than an empty panel, because a client that silently renders nothing
 * is indistinguishable from one with nothing to report.
 */
export function guided(): Promise<GuidedView> {
  return invoke<GuidedView>('guided');
}
