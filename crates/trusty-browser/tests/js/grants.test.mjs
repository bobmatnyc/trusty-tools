// Worker grant table across a quiet broker link (#11570): only the broker ends a grant.
// Run: node --test crates/trusty-browser/tests/js/*.test.mjs
import assert from "node:assert/strict";
import test from "node:test";

import {codeFixture, connectGrant, settle} from "./fixture.mjs";

// The native port closes and the broker stays silent for 31 minutes while the browser stays open.
const quiet = async f => {
  f.native.onDisconnect.fire();
  await settle();
  // Far past the 105 s quiet period the worker used to wipe its table after (b67da998cf).
  f.sessionStore.offline_since = Date.now() - 31 * 60 * 1000;
  await f.chrome.alarms.onAlarm.fire({name:"reconnect"});
  await settle();
};

test("a quiet broker link alone never ends a grant while the browser is open", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  const removed = f.perms.removed;
  await quiet(f);
  assert.deepEqual([f.perms.held, f.perms.removed, Object.keys(f.table())], [true, removed, [grant]]);
  // The broker answers again and still lists the grant: it works, with no UnknownGrant.
  await f.handshake();
  const r = await f.command("page_tabs", {grant});
  assert.equal(r.ok, true, JSON.stringify(r));
  assert.deepEqual([f.perms.held, Object.keys(f.table())], [true, [grant]]);
});

test("a broker that returns without the grant ends the row and the permission", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  await quiet(f);
  // A browser restart or a rejected grant store: the acknowledgement lists nothing live.
  f.live.grants = [];
  await f.handshake([]);
  assert.deepEqual([f.perms.held, f.table()], [false, {}]);
  const r = await f.command("page_tabs", {grant});
  assert.equal(r.error, "UnknownGrant", JSON.stringify(r));
});
