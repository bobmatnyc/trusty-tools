import assert from "node:assert/strict";
import {readFile} from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const source=await readFile(new URL("../src/trusty_browser/extension/content.js",import.meta.url),"utf8");
const account="personal",url="http://127.0.0.1:4444/qa/app";
function fixture() {
  let listener, cooked=account, switchOnClick=false;
  const received=[];
  const doc=new EventTarget();
  class Input extends EventTarget {}
  class Button extends EventTarget {
    dataset={};
    click() {
      if (switchOnClick) cooked="travel";
      if (this.dataset.expectedAccount === cooked) {
        received.push(cooked);
        doc.dispatchEvent(new Event("fixture-applied"));
      } else doc.dispatchEvent(new Event("fixture-refused"));
    }
  }
  const field=new Input(),button=new Button();
  field.value="";
  doc.querySelector=name=>({"#account":{textContent:account},"#field":field,"#count":{textContent:"0"},"#apply":button})[name];
  const runtime={id:"id",connect:()=>({onDisconnect:{addListener(){}}}),
    onMessage:{addListener(fn){listener=fn;}}};
  const page={};
  const fetch=async (endpoint,init)=>{
    assert.equal(endpoint.href,"http://127.0.0.1:4444/qa/identity");
    assert.equal(init.credentials,"same-origin");
    assert.equal(init.cache,"no-store");
    return {ok:true,json:async()=>({account:cooked})};
  };
  vm.runInNewContext(source,{chrome:{runtime},document:doc,window:page,top:page,location:{href:url},
    fetch,URL,TextEncoder,HTMLInputElement:Input,HTMLButtonElement:Button,Event,setTimeout,clearTimeout});
  const call=(op)=>new Promise(resolve=>listener({op,url,account,marker:"run"},{id:"id"},resolve));
  return {call,received,field,swap(){cooked="travel";},swapOnClick(){switchOnClick=true;}};
}
test("site preview and snapshot revalidate active cookie subject even when the DOM still names the prior account",async()=>{
  const f=fixture();
  assert.equal((await f.call("preview")).account,"personal");
  assert.equal((await f.call("snapshot")).ok,true);
  f.swap();
  assert.equal((await f.call("snapshot")).error,"SiteAccountMismatch");
  assert.equal((await f.call("click")).error,"SiteAccountMismatch");
  assert.deepEqual(f.received,[]);
});
test("a cookie switch after verification cannot commit a cross-account click",async()=>{
  const f=fixture();
  assert.equal((await f.call("click")).ok,true);
  f.swapOnClick();
  assert.equal((await f.call("click")).error,"SiteAccountMismatch");
  assert.deepEqual(f.received,["personal"]);
});

test("snapshot rejects multi-byte or JSON-expanded field values before leaving the renderer", async () => {
  const f=fixture();
  for (const value of ["å".repeat(33000),"\u0000".repeat(12000),"x".repeat(65537)]) {
    f.field.value=value;
    const reply=await f.call("snapshot");
    assert.equal(reply.error,"SnapshotTooLarge");
    assert.ok(!("result" in reply));
  }
});
