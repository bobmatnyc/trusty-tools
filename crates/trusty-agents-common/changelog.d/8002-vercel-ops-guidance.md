Fixed
- `vercel-ops` agent guidance now states which operations a Developer-role
  Vercel token cannot perform (Production env writes; a Production
  `vercel env ls` can return zero rows without meaning data was lost) and
  requires every `vercel env ls` to name an explicit environment instead of
  the unfiltered form.
