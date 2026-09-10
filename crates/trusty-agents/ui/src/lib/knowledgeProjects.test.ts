import { describe, expect, it } from 'vitest';
import { assistantProjectChats } from './knowledgeProjects';
const alpha = { id: 'a', name: 'Alpha', path: '/alpha', available: true };
const beta = { id: 'b', name: 'Beta', path: '/beta', available: true };
describe('assistant knowledge attachment projection', () => {
  it('preserves all local chats for this assistant without sharing another assistant’s attachments', () => {
    const chats = {
      '["first","izzie"]': { ids: ['a'], primary: 'a' },
      '["second","izzie"]': { ids: ['b'], primary: 'b' },
      '["first","other"]': { ids: ['a', 'b'], primary: 'a' },
      '["first",null]': { ids: ['a'], primary: 'a' },
    };
    expect(assistantProjectChats({ projects: [alpha, beta], chats }, 'izzie')).toEqual([
      { chat_id: '["first","izzie"]', projects: ['/alpha'] },
      { chat_id: '["second","izzie"]', projects: ['/beta'] },
    ]);
  });
  it('keeps explicit empty chats for detach sync and excludes unavailable, duplicate, and unknown roots', () => {
    expect(assistantProjectChats({ projects: [alpha, { ...alpha, id: 'alias' }, { ...beta, available: false }], chats: {
      '["a","izzie"]': { ids: ['a', 'alias', 'b', 'missing'], primary: 'a' },
      '["b","izzie"]': { ids: [], primary: null },
      malformed: { ids: ['a'], primary: 'a' },
    } }, 'izzie')).toEqual([
      { chat_id: '["a","izzie"]', projects: ['/alpha'] },
      { chat_id: '["b","izzie"]', projects: [] },
    ]);
  });
  it('does not create chat scopes from the project catalog or null assistant selection', () => {
    const value = { projects: [alpha, beta], chats: {} };
    expect(assistantProjectChats(value, 'izzie')).toEqual([]);
    expect(assistantProjectChats(value, null)).toEqual([]);
  });
});
