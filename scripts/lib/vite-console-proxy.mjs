// vite-console-proxy.mjs — `vite dev` proxy rows that reach the console (#9792).
//
// Why: the console's loopback listener refuses any request whose `Host` is not
//   its own `ip:port` / `localhost:port`, or whose `Origin` is not
//   `http://<Host>`. A Vite dev server on :5173-:5175 would forward the
//   browser's `Host: localhost:517x` and `Origin: http://localhost:517x`, and
//   the console would refuse both. Rewriting `Host` to the target and dropping
//   the dev server's OWN `Origin` makes the dev server a non-browser client of
//   the console, which is what it is. A foreign `Origin` is forwarded intact so
//   the console's write guard refuses it; dropping it would let any web page
//   issue a no-cors write through the proxy as an Origin-less request.
// What: `consoleProxy(target, paths)` returns one Vite `server.proxy` row per
//   path, each `{ target, changeOrigin: true, configure }`, where `configure`
//   removes `Origin` from a forwarded request only when it equals
//   `http://<the request's Host>`, the dev server's own origin.
// Test: `src/lib/devProxy.test.js` in ui-memory, ui-search and ui-analyze.

/** Drop the dev server's own `Origin`; forward any other one unchanged. */
export function dropOrigin(proxy) {
  proxy.on('proxyReq', (proxyReq, req) => {
    const { origin, host } = req.headers;
    if (origin !== undefined && origin === `http://${host}`) {
      proxyReq.removeHeader('origin');
    }
  });
}

/** One proxy row per path, all forwarding to `target` (#9792). */
export function consoleProxy(target, paths) {
  return Object.fromEntries(
    paths.map((path) => [path, { target, changeOrigin: true, configure: dropOrigin }]),
  );
}
