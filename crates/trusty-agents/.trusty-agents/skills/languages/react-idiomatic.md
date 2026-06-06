---
name: react-idiomatic
tags: [language, idioms, react, frontend]
summary: Idiomatic React coding guidelines — 2024-2025
---

# Idiomatic React — 2024-2025

## Core Philosophy
Idiomatic React in 2024-2025 is Server-Components-by-default, minimal-client-state, TanStack-Query for server state, Zustand for client state, TypeScript-first, and tested through React Testing Library. The community optimizes for less JavaScript shipped to the browser, derived state over duplicated state, and behavior-first tests.

## Idioms: DO / DON'T / WHY

**DO** default to Server Components in Next.js App Router; add `"use client"` only when the component needs interactivity, browser APIs, or client-only hooks. **DON'T** mark everything `"use client"` out of habit. **WHY**: Server Components ship zero JS to the browser and can fetch data directly without an API layer.

**DO** use TanStack Query (`@tanstack/react-query`) for all server-fetched data on the client. **DON'T** fetch in `useEffect` and store in `useState` for async server data. **WHY**: TanStack Query handles caching, deduplication, background refresh, retries, and loading/error states declaratively.

**DO** use Zustand for global client state when you need it. Keep stores small and focused. **DON'T** reach for Redux on new projects unless the codebase already uses it. **WHY**: Zustand is ~1KB, has no boilerplate, works outside the React tree, and is conceptually simpler than Redux Toolkit.

**DO** compute derived state inline in the render body (or with a memoized selector). **DON'T** use `useEffect` to sync derived state into a separate `useState`. **WHY**: derived `useEffect` chains create extra renders and stale-state bugs; React 19's `use()` and the React Compiler make manual derivation cheap.

**DO** use named function declarations for components: `export function UserCard({ name }: Props) { ... }`. **DON'T** use `export const UserCard: FC<Props> = ...`. **WHY**: `FC` implicitly added `children` (removed in React 18); function declarations show better in stack traces and don't need the `FC` import.

**DO** type event handlers precisely: `(e: React.ChangeEvent<HTMLInputElement>) => void`. **DON'T** annotate them as `(e: any) => void`. **WHY**: precise types catch element-mismatch bugs at compile time.

**DO** test with React Testing Library + `userEvent` (v14+). Test what the user sees and does. **DON'T** use Enzyme, and don't assert against component state or instance methods. **WHY**: testing internals couples tests to implementation; refactors silently break otherwise-correct code.

**DO** use `useId()` for stable IDs on form/aria attributes. **DON'T** generate IDs with `Math.random()` or a module-level counter inside a component body. **WHY**: `useId` is stable across renders and SSR-safe.

**DO** colocate component, test, and styles: `UserCard/UserCard.tsx`, `UserCard/UserCard.test.tsx`, `UserCard/UserCard.module.css`. **DON'T** mirror the source tree under a separate `__tests__/` root. **WHY**: colocation makes related files trivial to find, move, and delete together.

**DO** lift server fetches to Server Components and pass data as props. **DON'T** fetch the same data in both a Server Component and a child Client Component. **WHY**: Next.js dedupes Server Component fetches; double-fetching wastes work and risks cache-skew.

**DO** keep prop drilling shallow. Use context (or a state manager) when you need to pass data through more than two intermediate layers. **DON'T** thread props through 4+ components that don't use them. **WHY**: middle components become brittle to prop renames and shape changes.

**DO** use `useCallback` / `useMemo` only when profiling shows a problem, or when stability is required by a downstream `useEffect`/memoized child. **DON'T** wrap every function in `useCallback` reflexively. **WHY**: these hooks have their own overhead and the React Compiler will likely make most manual usage obsolete.

## Toolchain
- **Build**: Next.js 14+/15 (App Router), or Vite for SPAs
- **Test runner**: Vitest + React Testing Library + `@testing-library/user-event`
- **Server state**: `@tanstack/react-query` v5
- **Client state**: `zustand` (or Jotai for atom-style)
- **Forms**: `react-hook-form` + `zod` resolver; or React 19 server actions
- **Styling**: Tailwind v4 / CSS Modules — avoid CSS-in-JS for SSR-heavy apps

## Anti-Patterns to Reject
- `useEffect` for derived state — compute inline.
- Unconditional `"use client"` directives on every file.
- Prop drilling more than 2 levels — use context or a state manager.
- Fetching server data in `useEffect` + `useState` — use TanStack Query or Server Components.
- `React.FC` typed function components — use plain function declarations.
- Testing component state via `wrapper.state()` (Enzyme idiom) — test rendered output instead.
- Reflexive `useCallback`/`useMemo` everywhere — they have a cost.

## 2024-2025 Updates
- React 19 (stable 2024) added the `use()` hook for unwrapping Promises, plus `useActionState` and `useFormStatus`.
- React Compiler (beta 2024) auto-memoizes; manual `useMemo`/`useCallback` may become obsolete for most cases.
- Next.js 15 stabilized async Server Component params and `next/form` for action-driven forms.
- `next/form` and Server Actions reduced the need for client-side form-submission code.
- Jotai gained traction for atomic state, but Zustand remains the dominant choice for global stores.
