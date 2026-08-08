Security

- **`GET /api/events` no longer streams conversation content to unauthenticated
  callers** ([#5052](https://github.com/bobmatnyc/trusty-tools/issues/5052)).
  The SSE stream was exempt from bearer auth outright, on the grounds that a
  browser `EventSource` cannot send an `Authorization` header. A loopback bind
  did not contain that: any web page open in the operator's browser could call
  `new EventSource("http://127.0.0.1:7654/api/events")` and read
  `PmThinking.text`, `AgentMessage.text`, and `PmDelegating.task_preview`
  cross-origin — no shell access, no token, no non-loopback opt-in, and no log
  entry. The shipped Tauri sidecar starts that port by default.

  The stream now requires either a matching `Authorization: Bearer <token>` or a
  short-lived ticket minted at the new `POST /api/events/ticket`, which a
  browser can put in the `EventSource` URL. Minting is a write method, so it is
  behind the router-wide same-origin guard even when no operator token is
  configured — which is what closes the shipped default. A ticket that leaks
  into an access log or `Referer` grants five minutes of read-only stream
  access, not the whole `/api/*` write surface.

  `/api/health` and `/api/config` stay exempt deliberately: the sidecar polls
  health before it holds any credential, and `/api/config` publishes only the
  `auth_required` boolean that any 401 already reveals.

- **The trusty-agents API no longer serves permissive CORS**
  ([#5052](https://github.com/bobmatnyc/trusty-tools/issues/5052)).
  `allow_origin(Any)` let a page on any origin read every GET response —
  `/api/tasks`, `/api/task/{id}`, `/api/sessions/{id}/recap` all carry
  conversation content, and the same-origin write guard did not cover them
  because it is method-gated. The daemon now reflects only same-machine origins:
  loopback (including a `pnpm dev` server on `localhost:5173`), the Tauri
  webview, and its own resolved non-loopback bind. Server-side callers — the
  trusty-console reverse proxy, `curl`, MCP stdio bridges — send no `Origin` and
  are unaffected.
