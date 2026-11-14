// Activation-code state in the worker (#11239, docs/activation-codes.md): the worker grant table, the
// optional https://*/* permission, the per-profile pairing and the popup's code flow. The worker loads this
// file with importScripts; top-level code touches no `chrome` API.
globalThis.TrustyCodes = deps => {
  const {bootstrap, identity, pageOps, reconnect} = deps;
  const ALL_HTTPS = {origins:["https://*/*"]};
  // CODE_TTL (300 s) plus a minute. A broker this worker has not reached for that long holds no code or grant
  // of the profile: every grant ends with its pairing generation, and every code expires.
  const QUIET_MS = 360000;
  const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
  const HEX64 = /^[0-9a-f]{64}$/;
  const tabId = value => Number.isInteger(value) && value >= 0;
  const object = value => value && typeof value === "object" ? value : {};
  const watchers = new Set();
  let chain = Promise.resolve(), minting = 0, busy = false;

  function serial(task) {
    const step = chain.then(task);
    chain = step.catch(() => {});
    return step;
  }
  async function readTable() {
    await chain;
    return object((await chrome.storage.session.get("grants")).grants);
  }
  async function dropPermission() {
    try { await chrome.permissions.remove(ALL_HTTPS); } catch { /* The next signal removes it. */ }
  }
  // Why: https://*/* must not outlive the profile's last code or grant, and must not end before it.
  // What: applies `edit` to the stored {grant: {scope, tab}} table and sets the badge. Only with `none` (the
  //   broker reported that the profile holds nothing live) does it remove the permission, and then only when
  //   the table is empty and no mint is in flight; a failed table write still removes it.
  // Test: tests/test_worker.mjs `grant_end with last true removes the permission and last false keeps it`,
  //   `the pairing acknowledgement prunes the table to its live list`.
  function changeTable(edit, none = false) {
    return serial(async () => {
      const next = edit({...object((await chrome.storage.session.get("grants")).grants)});
      let failure = null;
      try { await chrome.storage.session.set({grants:next}); } catch (error) { failure = error; }
      try { await TrustyControlIndicator.grants?.(Object.keys(next).length); } catch { /* The badge is advisory. */ }
      if (none && !Object.keys(next).length && !minting) await dropPermission();
      if (failure) throw failure;
      return next;
    });
  }
  function watchMints() {
    const fresh = new Set();
    watchers.add(fresh);
    return fresh;
  }
  // Keeps the rows a broker list names, plus rows minted while that list was in flight.
  function keepListed(listed, fresh, none) {
    watchers.delete(fresh);
    return changeTable(t => Object.fromEntries(Object.entries(t).filter(([g]) => listed.has(g) || fresh.has(g))), none);
  }
  async function stored() {
    const {enrollment, pairing, pairing_pending:pending} = await chrome.storage.local.get(["enrollment","pairing","pairing_pending"]);
    return {enrollment, pairing, pending, credential:enrollment || pairing || null};
  }
  async function credential() {
    const {credential:found} = await stored();
    if (!found) throw new Error("NotEnrolled");
    const email = await identity();
    if (found.email !== email) throw new Error("The activated profile changed.");
    return {profile:found.profile, email, key:found.key};
  }
  async function listGrants(cred) {
    const fresh = watchMints();
    let reply;
    try { reply = await bootstrap({op:"grants", ...cred}); } finally {
      if (!Array.isArray(reply?.grants)) watchers.delete(fresh);
    }
    if (!Array.isArray(reply.grants)) throw new Error("Invalid grants reply");
    const listed = new Set(reply.grants.map(entry => entry?.grant));
    await keepListed(listed, fresh, listed.size === 0);
    return reply.grants;
  }
  // Why: a failed mint, or a `last` end beside a row the broker did not count, gets no later contract signal.
  // What: asks the broker for the profile's list and prunes to it, dropping the permission when it is empty.
  //   No credential, or a broker that matches none, means nothing can be live. An unreachable broker
  //   changes nothing; the offline check settles that case.
  // Test: tests/test_worker.mjs `failure branches never leave a grant usable or a permission held`.
  async function reconcile() {
    try { await listGrants(await credential()); } catch (error) {
      if ((error.response?.error || error.message) === "NotEnrolled") await changeTable(() => ({}), true).catch(() => {});
    }
  }
  async function ended(grant, last) {
    let next = null;
    try { next = await changeTable(t => { delete t[grant]; return t; }, last); } catch { /* Settled below. */ }
    // A row left beside `last` is a mint racing this end or a missed end; the broker's list tells which.
    if (last && (next === null || Object.keys(next).length)) void reconcile();
  }
  const offline = {
    lost:() => serial(async () => {
      const {offline_since:since} = await chrome.storage.session.get("offline_since");
      if (!Number.isFinite(since)) await chrome.storage.session.set({offline_since:Date.now()});
    }).catch(() => {}),
    minted:() => serial(async () => {
      const {offline_since:since} = await chrome.storage.session.get("offline_since");
      if (Number.isFinite(since)) await chrome.storage.session.set({offline_since:Date.now()});
    }).catch(() => {}),
  };
  // The service.sock acknowledgement: `live` lists every unredeemed code and live grant of the profile.
  async function acknowledged(live, fresh) {
    await serial(() => chrome.storage.session.set({offline_since:null})).catch(() => {});
    await keepListed(new Set(live), fresh, live.length === 0);
  }
  // Why: a broker that stays unreachable sends no acknowledgement, yet then holds nothing of this profile.
  // What: after QUIET_MS without an acknowledgement and without a mint, empties the table and the permission.
  // Test: tests/test_worker.mjs `an unreachable broker ends the table and the permission after the code lifetime`.
  async function tick() {
    const {offline_since:since} = await chrome.storage.session.get("offline_since");
    if (Number.isFinite(since) && Date.now() - since >= QUIET_MS) await changeTable(() => ({}), true);
  }

  async function mint(msg, cred) {
    const {scope} = msg;
    if (!["tab","profile"].includes(scope) || (scope === "tab") !== Object.hasOwn(msg, "tab") ||
        (scope === "tab" && !tabId(msg.tab))) throw new Error("InvalidMint");
    if (scope === "tab" && !(await pageOps().probe(msg.tab)).access) throw new Error("PermissionNeeded");
    minting++;
    let reply;
    try {
      reply = await bootstrap({op:"mint", ...cred, scope, ...(scope === "tab" ? {tab:msg.tab} : {})});
      const {code, grant, expires_at} = reply;
      if (typeof grant !== "string" || !/^g-[0-9a-f]{1,64}$/.test(grant)) throw new Error("Invalid mint reply");
      try {
        if (typeof code !== "string" || !/^tb-[0-9A-Z]{6}(-[0-9A-Z]{6}){3}$/.test(code) || reply.scope !== scope ||
            (scope === "tab" && reply.tab !== msg.tab) || !Number.isFinite(expires_at)) throw new Error("Invalid mint reply");
        for (const fresh of watchers) fresh.add(grant);
        await changeTable(t => ({...t, [grant]:{scope, tab:scope === "tab" ? msg.tab : null}}));
      } catch (error) {
        // A code the worker cannot honour is cancelled rather than left redeemable.
        try { await bootstrap({op:"revoke_grant", ...cred, grant}); } catch { /* It expires within CODE_TTL. */ }
        throw new Error(error.message === "Invalid mint reply" ? error.message : "Could not record the grant.");
      }
    } catch (error) {
      minting--;
      await reconcile();
      throw error;
    }
    minting--;
    await offline.minted();
    return {ok:true, code:reply.code, grant:reply.grant, scope, profile:reply.profile,
      ...(scope === "tab" ? {tab:msg.tab} : {}), expires_at:reply.expires_at};
  }

  // Why: a profile with no workflow enrollment needs its own credential to mint codes (contract "pair").
  // What: keeps one `pairing_pending` triple across interruptions, sends `pair`, and on `ok` moves it to
  //   `pairing` and reconnects. A failure that leaves no credential also leaves no permission.
  // Test: tests/test_worker.mjs `pair records the pairing only on ok and retries with the same fields`.
  async function pair() {
    const {enrollment, pairing, pending} = await stored();
    const {pending:workflow} = await chrome.storage.local.get("pending");
    if (enrollment || workflow) throw new Error("This profile already has a workflow enrollment.");
    if (pairing) {
      if (pending) await chrome.storage.local.remove("pairing_pending");
      return {ok:true, profile:pairing.profile};
    }
    const email = await identity();
    const data = pending?.email === email && UUID.test(pending.profile) && HEX64.test(pending.key)
      ? {profile:pending.profile, email, key:pending.key}
      : {profile:crypto.randomUUID(), email, key:Array.from(crypto.getRandomValues(new Uint8Array(32)),
        x => x.toString(16).padStart(2, "0")).join("")};
    await chrome.storage.local.set({pairing_pending:data});
    let reply;
    try { reply = await bootstrap({op:"pair", ...data}); } catch (error) {
      // A refusal from the verified host is definite; any other failure keeps the fields for the retry.
      if (error.response) await chrome.storage.local.remove("pairing_pending");
      throw error;
    }
    if (reply.profile !== data.profile) throw new Error("Invalid pairing reply");
    await chrome.storage.local.set({pairing:data});
    await chrome.storage.local.remove("pairing_pending");
    reconnect();
    return {ok:true, profile:data.profile};
  }
  // Why: Disconnect this profile ends every code and grant and the pairing itself (contract "pair").
  // What: marks the pairing before sending `unpair`, so a retry after a lost reply treats Unauthorized as
  //   done; then empties the table, removes the permission and the pairing, and reconnects.
  // Test: tests/test_worker.mjs `unpair removes the permission and the pairing record`.
  async function unpair() {
    const {pairing} = await stored();
    if (!pairing) throw new Error("This profile is not activated.");
    const {profile, email, key} = pairing;
    const retry = pairing.unpairing === true;
    if (!retry) await chrome.storage.local.set({pairing:{...pairing, unpairing:true}});
    try { await bootstrap({op:"unpair", profile, email, key}); } catch (error) {
      if (!(retry && error.response?.error === "Unauthorized")) throw error;
    }
    await changeTable(() => ({})).catch(() => {});
    await dropPermission();
    await chrome.storage.local.remove(["pairing","pairing_pending"]);
    reconnect();
    return {ok:true, profile};
  }
  async function exclusive(task) {
    if (busy) throw new Error("Activation already in progress.");
    busy = true;
    try { return await task(); } catch (error) {
      if (!(await stored()).credential) await changeTable(() => ({}), true).catch(() => {});
      throw error;
    } finally { busy = false; }
  }

  // Why: the popup's code flow — preview, pair, mint, list and revoke.
  // What: routes one popup message. No path reads the active tab; `tab` is always the popup's explicit id.
  // Test: tests/test_worker.mjs `code preview probes the tab the popup names and never the active tab`,
  //   `a mint without a pairing is refused`.
  async function setup(msg) {
    if (msg.type === "code_preview") {
      if (msg.tab !== null && !tabId(msg.tab)) throw new Error("InvalidRequest");
      const {pairing, credential:found} = await stored();
      const email = await identity();
      const seen = msg.tab === null ? null : await pageOps().probe(msg.tab);
      return {ok:true, paired:!!pairing, email, profile:found?.profile ?? null,
        target:seen && {tab:msg.tab, access:seen.access, origin:seen.origin, title:seen.title}};
    }
    if (msg.type === "pair") return exclusive(pair);
    if (msg.type === "unpair") return exclusive(unpair);
    const cred = await credential();
    if (msg.type === "mint") return mint(msg, cred);
    if (msg.type === "grants") {
      const grants = await Promise.all((await listGrants(cred)).map(async entry => {
        if (entry?.scope !== "tab" || !tabId(entry.tab)) return entry;
        const seen = await pageOps().probe(entry.tab);
        return {...entry, origin:seen.origin, title:seen.title};
      }));
      return {ok:true, grants};
    }
    if (msg.type === "revoke_grant") {
      if (typeof msg.grant !== "string" || !msg.grant) throw new Error("InvalidRequest");
      const drop = () => changeTable(t => { delete t[msg.grant]; return t; });
      let reply;
      try { reply = await bootstrap({op:"revoke_grant", ...cred, grant:msg.grant}); } catch (error) {
        if (error.response?.error === "UnknownGrant") await drop();
        throw error;
      }
      await drop();
      return {ok:true, revoked:reply.revoked};
    }
    const {revoked} = await bootstrap({op:"revoke_grants", ...cred});
    await changeTable(() => ({}));
    return {ok:true, revoked};
  }
  return {setup, credential, stored, readTable, ended, acknowledged, watchMints,
    unwatch:fresh => watchers.delete(fresh), offline, tick};
};
