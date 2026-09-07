<script>
  /*
   * Why: Operators debugging a running daemon want recent logs in the browser
   * without SSHing to the host.
   * What: Thin wrapper around the shared `LogViewer` component, wired to
   * trusty-memory's `GET /api/v1/logs/tail` endpoint.
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
