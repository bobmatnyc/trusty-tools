import { afterEach, expect, it, vi } from 'vitest';
import { mount, tick, unmount } from 'svelte';
vi.mock('../lib/api-config', () => ({ apiBase: () => '' }));
vi.mock('../stores/app', () => ({ getCurrentApiToken: () => 'fixture-token' }));
import StoredChatImage from './StoredChatImage.svelte';
let component: ReturnType<typeof mount> | undefined;
const additional: ReturnType<typeof mount>[] = [];
afterEach(async () => { if (component) await unmount(component); for (const child of additional.splice(0)) await unmount(child); component = undefined; document.body.innerHTML = ''; vi.unstubAllGlobals(); });
it('loads a scoped authenticated image and revokes the preview URL on unmount', async () => {
  const createObjectURL = vi.fn().mockReturnValue('blob:fixture'), revokeObjectURL = vi.fn();
  vi.stubGlobal('URL', { createObjectURL, revokeObjectURL });
  const fetcher = vi.fn().mockResolvedValue(new Response(new Uint8Array([137,80,78,71]), { headers: {'Content-Type':'image/png'} }));
  vi.stubGlobal('fetch', fetcher);
  let visible!: (entries: {isIntersecting: boolean}[]) => void;
  vi.stubGlobal('IntersectionObserver', class { constructor(callback: typeof visible) { visible = callback; } observe() {} disconnect() {} });
  component = mount(StoredChatImage, { target: document.body, props: { assistant: 'alice name', attachment: {name:'Saved chart',mime_type:'image/png',asset_id:'opaque/id'} } });
  await tick();
  expect(fetcher).not.toHaveBeenCalled();
  visible([{isIntersecting:true}]);
  await vi.waitFor(() => expect(createObjectURL).toHaveBeenCalledTimes(1)); await tick();
  expect(fetcher).toHaveBeenCalledWith('/api/agents/alice%20name/chat-assets/opaque%2Fid', expect.objectContaining({ headers: {Authorization:'Bearer fixture-token'} }));
  expect(document.querySelector('img')?.getAttribute('src')).toBe('blob:fixture');
  visible([{isIntersecting:false}]); await tick();
  expect(document.querySelector('img')).toBeNull();
  expect(revokeObjectURL).toHaveBeenCalledWith('blob:fixture');
  await unmount(component); component = undefined;
  expect(revokeObjectURL).toHaveBeenCalledWith('blob:fixture');
});
it('offers an explicit reload after a nearby preview is evicted for another image', async () => {
  const callbacks: ((entries: {isIntersecting: boolean}[]) => void)[] = [];
  vi.stubGlobal('IntersectionObserver', class { constructor(callback: typeof callbacks[number]) { callbacks.push(callback); } observe() {} disconnect() {} });
  let serial = 0;
  vi.stubGlobal('URL',{createObjectURL:vi.fn(() => `blob:image-${serial++}`),revokeObjectURL:vi.fn()});
  const fetcher = vi.fn().mockImplementation(() => Promise.resolve(new Response(new Uint8Array([137,80,78,71]),{headers:{'Content-Type':'image/png'}})));
  vi.stubGlobal('fetch',fetcher);
  for (let i = 0; i < 5; i++) {
    const target = document.createElement('div'); document.body.appendChild(target);
    additional.push(mount(StoredChatImage,{target,props:{assistant:'alice',attachment:{name:`Chart ${i}`,mime_type:'image/png',asset_id:String(i)}}}));
  }
  await tick(); callbacks.forEach(callback => callback([{isIntersecting:true}]));
  await vi.waitFor(() => expect(fetcher).toHaveBeenCalledTimes(5)); await tick();
  const first = document.body.firstElementChild!;
  await vi.waitFor(() => expect(first.textContent).toContain('Preview released'));
  (first.querySelector('button') as HTMLButtonElement).click();
  await vi.waitFor(() => expect(fetcher).toHaveBeenCalledTimes(6));
  await vi.waitFor(() => expect(first.querySelector('img')).not.toBeNull());
});
