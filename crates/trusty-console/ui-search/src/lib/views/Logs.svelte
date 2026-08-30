<script>
  /*
   * Why: Operators debugging a running daemon want recent logs in the browser
   * without SSHing to the host. The TUI has no log pane, so this is a
   * browser-only superset view.
   * What: Thin wrapper around the shared `LogViewer` component, wired to
   * trusty-search's `GET /logs/tail` endpoint.
   * Test: open #/logs, confirm log lines render and the level filter works.
   */
  import LogViewer from '../components/LogViewer.svelte';
  import { api } from '../api.js';
</script>

<h1 class="page-title">Logs</h1>

<LogViewer fetchLogs={() => api.logsTail(200)} />

<style>
  .page-title {
    font-size: var(--trusty-fs-xl);
    margin: 0 0 var(--trusty-space-5) 0;
    font-weight: 600;
  }
</style>
