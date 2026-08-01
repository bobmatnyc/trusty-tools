/**
 * Why (#4359): the markdown editor needs somewhere to read from and write to,
 * and every component that grows its own `fetch` for project files ends up
 * re-deriving the same three things badly — the route shape, "is this file
 * editable", and what to do when the daemon says no. Keeping them in one
 * framework-free module means the editor stays presentational, the panel stays
 * thin, and the gating question has exactly one place to grow when #4360
 * widens it from "markdown" to a full document-type table.
 *
 * What: the typed client for the per-project file routes defined by #4357
 * (`GET  /api/projects/:id/files/list`, `GET .../files/read`,
 * `POST .../files/write`), plus `DOCUMENT_TYPES` — the document-type table
 * (#4360) that is the single answer to "what may this file do here".
 *
 * ROUTE AVAILABILITY: #4357 owns the server side of these three routes and has
 * not landed yet. This module therefore does NOT fake a response when they are
 * absent — `tmApi` throws the daemon's status through verbatim and the caller
 * renders it (see `ProjectFilesPanel.svelte`). A silent local fallback here
 * would make an unimplemented backend look like an empty project directory,
 * which is the failure mode the repo's no-silent-fallback rule exists to stop.
 *
 * ONE TABLE, TWO CONSUMERS (#4360): `writeProjectFile`'s refusal and
 * `ProjectFilesPanel`'s row gating both resolve through `documentKindFor`, so
 * the enumeration cannot drift between what the UI offers and what the client
 * will send. A second list of "editable extensions" anywhere in the app is a
 * bug by construction — widen the table instead.
 *
 * Test: `projectFiles.test.ts`.
 */
import { tmApi } from '../stores/app';

/**
 * What a document may do in the Projects surface.
 *
 * - `editable`   — opens in `MarkdownEditor` and may be written back.
 * - `view-only`  — a known document type that must never be written. It has no
 *                  viewer yet; #4401 owns that surface.
 * - `unsupported` — not a document this surface handles. Refused outright.
 */
export type DocumentKind = 'editable' | 'view-only' | 'unsupported';

/** One row of the document-type table. */
export interface DocumentType {
  /** Lowercase extension, leading dot included. */
  extension: string;
  /** Everything enumerated is either editable or view-only, never unsupported. */
  kind: Exclude<DocumentKind, 'unsupported'>;
  /** Human name, used verbatim in refusal messages and list rows. */
  label: string;
}

/**
 * The enumeration #4360 asks for: every document type this surface handles.
 *
 * ONLY MARKDOWN IS EDITABLE — that is the parent requirement, not an
 * implementation detail, so `.txt` and `.csv` sit under `view-only` despite
 * being trivially writable text. Widening editability is an owner decision.
 *
 * `.markdown` is here because the CommonMark-era long form still shows up in
 * older docs trees; it maps to the same editor as `.md`.
 *
 * The view-only set is exactly what `trusty-search`'s extractor already
 * understands (`core/extract/mod.rs`, #2923) plus the two plain-text formats,
 * so a viewer can be built on extraction that already exists rather than on a
 * type nobody can read.
 *
 * DELIBERATELY ABSENT, pending the owner call the issue asks for: `.pptx`,
 * images (`.png`/`.jpg`/`.gif`), and `.rst`/`.org`. Absent means `unsupported`
 * means refused — the safe default, and adding a row later is additive.
 */
export const DOCUMENT_TYPES: readonly DocumentType[] = [
  { extension: '.md', kind: 'editable', label: 'Markdown' },
  { extension: '.markdown', kind: 'editable', label: 'Markdown' },
  { extension: '.pdf', kind: 'view-only', label: 'PDF' },
  { extension: '.docx', kind: 'view-only', label: 'Word document' },
  { extension: '.xlsx', kind: 'view-only', label: 'Excel workbook' },
  { extension: '.xls', kind: 'view-only', label: 'Excel workbook' },
  { extension: '.txt', kind: 'view-only', label: 'Plain text' },
  { extension: '.csv', kind: 'view-only', label: 'CSV' },
] as const;

const BY_EXTENSION = new Map(DOCUMENT_TYPES.map((type) => [type.extension, type]));

/**
 * One entry of a `files/list` response — the metadata quartet #4357 specifies
 * (name, type, size, mtime). `size`/`modified` are nullable because a
 * directory entry has no meaningful size and a filesystem may refuse mtime.
 */
export interface ProjectFileEntry {
  /** Base name, no directory component. */
  name: string;
  /** Path relative to the registered project root — what the routes take back. */
  path: string;
  is_dir: boolean;
  size: number | null;
  /** RFC-3339 timestamp, or null when the filesystem did not report one. */
  modified: string | null;
}

/** Response envelope of `GET /api/projects/:id/files/read`. */
export interface ProjectFileContent {
  path: string;
  content: string;
}

/**
 * Lowercase extension of `path`, or `''` when it has none.
 *
 * Reads the extension off the base name rather than the whole path, so a
 * directory carrying a dot (`docs/v1.2/NOTES`) does not lend its suffix to a
 * file that has none. A leading dot is a dotfile (`.gitignore`), not an
 * extension, hence `dot <= 0`.
 */
function extensionOf(path: string): string {
  const base = path.slice(path.lastIndexOf('/') + 1);
  const dot = base.lastIndexOf('.');
  return dot <= 0 ? '' : base.slice(dot).toLowerCase();
}

/**
 * The table row governing `path`, or `null` when nothing in the table claims it.
 *
 * Case-insensitive: `README.MD` off a case-preserving filesystem is the same
 * document as `readme.md`.
 */
export function documentTypeFor(path: string): DocumentType | null {
  return BY_EXTENSION.get(extensionOf(path)) ?? null;
}

/** What `path` may do here. Everything the table does not claim is refused. */
export function documentKindFor(path: string): DocumentKind {
  return documentTypeFor(path)?.kind ?? 'unsupported';
}

/** True when `path` may be opened for editing and written back. */
export function isEditablePath(path: string): boolean {
  return documentKindFor(path) === 'editable';
}

/**
 * Why `path` cannot be edited, or `null` when it can.
 *
 * Shared by the write guard and the panel's row hints so a user reads the same
 * sentence whichever way they hit the gate — the wording is part of the
 * enumeration, not per-call-site prose.
 */
export function editRefusalReason(path: string): string | null {
  const type = documentTypeFor(path);
  if (type?.kind === 'editable') return null;
  return type
    ? `${type.label} documents are view-only — only markdown can be edited.`
    : `${extensionOf(path) || 'This file type'} is not a supported document type.`;
}

function filesUrl(projectId: string, op: string, params: Record<string, string> = {}): string {
  const query = new URLSearchParams(params).toString();
  const base = `/api/projects/${encodeURIComponent(projectId)}/files/${op}`;
  return query ? `${base}?${query}` : base;
}

/**
 * List one directory inside a registered project root.
 *
 * `path` is project-root-relative; `''` lists the root itself. Traversal
 * guards are the server's job (#4357) — sending `..` here earns a 400, which
 * is the correct outcome and must not be pre-empted by client-side guessing.
 */
export async function listProjectFiles(
  projectId: string,
  path = '',
): Promise<ProjectFileEntry[]> {
  const body = await tmApi<{ entries?: ProjectFileEntry[] } | ProjectFileEntry[]>(
    filesUrl(projectId, 'list', { path }),
  );
  return Array.isArray(body) ? body : (body.entries ?? []);
}

/** Read one file's contents from a registered project root. */
export async function readProjectFile(
  projectId: string,
  path: string,
): Promise<ProjectFileContent> {
  return tmApi<ProjectFileContent>(filesUrl(projectId, 'read', { path }));
}

/**
 * Write markdown back to a registered project root.
 *
 * Refuses anything the table does not call `editable` before the request
 * leaves the client: the contract is "only markdown is editable" (#4360), and
 * a caller that reaches here with a `.pdf` has a bug the server's 400 would
 * report too late to be useful. The server remains the authority — this is a
 * fast, specific failure, not a substitute for it.
 */
export async function writeProjectFile(
  projectId: string,
  path: string,
  content: string,
): Promise<void> {
  const refusal = editRefusalReason(path);
  if (refusal) {
    throw new Error(`${refusal} Refusing to write ${path}`);
  }
  await tmApi(filesUrl(projectId, 'write'), {
    method: 'POST',
    body: JSON.stringify({ path, content }),
  });
}
