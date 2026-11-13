(() => {
if (globalThis.trustyBrowserAdapterLoaded) return;
globalThis.trustyBrowserAdapterLoaded = true;
function registerDocument() {
  const port = chrome.runtime.connect({name: "qa-document"});
  port.onDisconnect.addListener(() => {
    if (chrome.runtime.id) setTimeout(registerDocument, 1000);
  });
}
registerDocument();

chrome.runtime.onMessage.addListener((request, sender, reply) => {
  if (request.type === "trusty-control-indicator" || sender.id !== chrome.runtime.id || window !== top) return;
  const renderedAccount = () => document.querySelector("#account")?.textContent.trim();
  const check = expected => {
    if (request.url !== location.href) throw new Error("RouteChanged");
    if (!expected || renderedAccount() !== expected) throw new Error("SiteAccountMismatch");
  };
  async function verifiedAccount() {
    if (request.url !== location.href) throw new Error("RouteChanged");
    const response = await fetch(new URL("identity", request.url), {
      credentials:"same-origin", mode:"same-origin", cache:"no-store", redirect:"error"
    });
    if (!response.ok) throw new Error("SiteAccountMismatch");
    const {account} = await response.json();
    if (typeof account !== "string" || !account || account.length > 512 ||
        (request.op !== "preview" && account !== request.account)) throw new Error("SiteAccountMismatch");
    check(account);
    return account;
  }
  const run = async () => {
    const field = document.querySelector("#field");
    const count = document.querySelector("#count");
    if (!(field instanceof HTMLInputElement) || !count) throw new Error("Unsupported site adapter");
    const account = await verifiedAccount();
    if (request.op === "click") {
      const button = document.querySelector("#apply");
      if (!(button instanceof HTMLButtonElement) || typeof request.marker !== "string" ||
          !request.marker || request.marker.length > 1024) throw new Error("InvalidClick");
      field.value = request.marker;
      field.dispatchEvent(new Event("input", {bubbles:true}));
      check(account);
      await new Promise((resolve, reject) => {
        const clear = () => {
          clearTimeout(timer);
          document.removeEventListener("fixture-applied", done);
          document.removeEventListener("fixture-refused", refused);
        };
        const done = () => { clear(); resolve(); };
        const refused = () => { clear(); reject(new Error("SiteAccountMismatch")); };
        const timer = setTimeout(() => { clear(); reject(new Error("OutcomeUnknown")); }, 4000);
        document.addEventListener("fixture-applied", done, {once:true});
        document.addEventListener("fixture-refused", refused, {once:true});
        // The fixture POST verifies this expected subject and its current cookie before committing.
        button.dataset.expectedAccount = account;
        button.click();
        delete button.dataset.expectedAccount;
      });
      await verifiedAccount();
    } else if (!["preview","inspect","snapshot"].includes(request.op)) throw new Error("InvalidOperation");
    check(account);
    // Bound encoded size before crossing the renderer boundary. JSON may expand a
    // control character to six bytes and a Unicode scalar to four UTF-8 bytes.
    const value = field.value;
    if (typeof value !== "string" || value.length > 65536 ||
        new TextEncoder().encode(JSON.stringify(value)).byteLength > 65536) {
      throw new Error("SnapshotTooLarge");
    }
    return {account,url:location.href,result:{count:Number(count.textContent),field:value}};
  };
  run().then(value => reply({ok:true, ...value}), error => reply({ok:false,error:error.message}));
  return true;
});
})();
