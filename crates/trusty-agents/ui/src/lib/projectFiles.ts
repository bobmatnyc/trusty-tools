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
 * `POST .../files/write`), plus `isMarkdownPath` — the single predicate this
 * slice gates writes on.
 *
 * ROUTE AVAILABILITY: #4357 owns the server side of these three routes and has
 * not landed yet. This module therefore does NOT fake a response when they are
 * absent — `tmApi` throws the daemon's status through verbatim and the caller
 * renders it (see `ProjectFilesPanel.svelte`). A silent local fallback here
 * would make an unimplemented backend look like an empty project directory,
 * which is the failure mode the repo's no-silent-fallback rule exists to stop.
 *
 * GATING SEAM (#4360): `isMarkdownPath` answers only "may this be edited in
 * this slice". #4360 replaces its call sites with a document-type table
 * (`markdown` → editable, `pdf`/`docx`/`xlsx` → view-only, rest → rejected);
 * nothing outside this module and `ProjectFilesPanel` needs to change, because
 * `MarkdownEditor` takes editability as a `readonly` prop rather than sniffing
 * the path itself.
 *
 * Test: `projectFiles.test.ts`.
 */
import { tmApi } from '../stores/app';

/**
 * Extensions treated as editable markdown by this slice.
 *
 * `.markdown` is included because the CommonMark-era long form still shows up
 * in older docs trees; both map to the same editor.
 */
export const MARKDOWN_EXTENSIONS = ['.md', '.markdown'] as const;

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
 * True when `path` names a markdown document.
 *
 * Case-insensitive: `README.MD` off a case-preserving filesystem is the same
 * document as `readme.md`.
 */
export function isMarkdownPath(path: string): boolean {
  const lower = path.toLowerCase();
  return MARKDOWN_EXTENSIONS.some((ext) => lower.endsWith(ext));
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
 * Refuses non-markdown before the request leaves the client: this slice's
 * contract is "only markdown is editable" (#4359), and a caller that reaches
 * here with a `.pdf` has a bug the server's 400 would report too late to be
 * useful. The server remains the authority — this is a fast, specific failure,
 * not a substitute for it.
 */
export async function writeProjectFile(
  projectId: string,
  path: string,
  content: string,
): Promise<void> {
  if (!isMarkdownPath(path)) {
    throw new Error(`Only markdown files are editable — refusing to write ${path}`);
  }
  await tmApi(filesUrl(projectId, 'write'), {
    method: 'POST',
    body: JSON.stringify({ path, content }),
  });
}
