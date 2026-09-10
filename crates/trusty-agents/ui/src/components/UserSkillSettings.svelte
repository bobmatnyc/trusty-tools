<script lang="ts">
  import { onMount } from 'svelte';
  import { userSkillSettings, loadUserSkills, toggleUserSkillSource } from '../lib/userSkillSettings';
  $: ({ data, loading, saving, error, notice, draft } = $userSkillSettings);
  onMount(() => { void loadUserSkills(); });
  const reload = () => loadUserSkills(true);
  const toggle = toggleUserSkillSource;
</script>

<section aria-label="User skills" class="user-skills">
  <h3>User skills</h3>
  <p>Available across your projects. A project skill with the same name takes precedence. These switches apply to all assistants; tool permissions are managed separately.</p>
  {#if loading}<p role="status">Loading user skills…</p>{/if}
  {#if error}<p role="alert">{error}</p>{/if}
  {#if data}
    {#if data.sources.length === 0}<p>No supported user skill folders found.</p>{/if}
    {#each data.sources as source (source.id)}
      <div class="source">
        <label><input type="checkbox" checked={draft[source.id]} disabled={loading || saving} on:change={event => toggle(source.id, event.currentTarget.checked)} /><strong>{source.label}</strong></label>
        <p class="path">{source.path}</p>
        {#if source.error}<p role="alert">{source.error}</p>{/if}
        <details><summary>{source.skills.length} {source.skills.length === 1 ? 'skill' : 'skills'} discovered</summary>
          {#each source.skills as skill (skill.path)}
            <div class="skill"><strong>{skill.name}</strong>{#if skill.description}<p>{skill.description}</p>{/if}<p class="path">{skill.path}</p></div>
          {/each}
        </details>
      </div>
    {/each}
  {/if}
  {#if saving}<p role="status">Saving user skills…</p>{/if}
  {#if notice}<p role="status">{notice}</p>{/if}
  <button type="button" disabled={loading || saving} on:click={reload}>Reload user skills</button>
</section>

<style>
  .user-skills { font-size:12px; padding:16px 0; border-bottom:1px solid rgb(var(--color-border) / .6); }
  h3 { font-weight:600; margin-bottom:8px; }
  p { margin:7px 0; opacity:.75; overflow-wrap:anywhere; }
  .source { padding:10px 0; } label { display:flex; align-items:center; gap:8px; }
  input { accent-color:rgb(var(--color-primary)); }
  .path { font-size:10px; } summary { cursor:pointer; font-size:11px; }
  .skill { padding:8px 0 8px 10px; border-left:1px solid rgb(var(--color-border)); margin-top:8px; }
  button { border:1px solid rgb(var(--color-border)); border-radius:5px; padding:7px 10px; margin-top:6px; cursor:pointer; }
  button:disabled { opacity:.45; cursor:default; }
  [role=alert] { color:rgb(var(--color-warning)); opacity:1; }
</style>
