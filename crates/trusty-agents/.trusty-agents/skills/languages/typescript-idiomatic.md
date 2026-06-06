---
name: typescript-idiomatic
tags: [language, idioms, typescript]
summary: Idiomatic TypeScript coding guidelines — 2024-2025
---

# Idiomatic TypeScript — 2024-2025

## Core Philosophy
Idiomatic TypeScript in 2024-2025 is strict-mode-on, ESM-first, runtime-validated at boundaries, biome-tooled, and vitest-tested. The community optimizes for compile-time guarantees through the type system and runtime guarantees through schema validators (Zod) — never trust unchecked external data.

## Idioms: DO / DON'T / WHY

**DO** enable `strict: true`, `noUncheckedIndexedAccess: true`, and `exactOptionalPropertyTypes: true` in `tsconfig.json`. **DON'T** disable strict checks to silence errors — fix the types instead. **WHY**: `noUncheckedIndexedAccess` eliminates a whole class of silent `undefined` errors that pure `strict` misses.

**DO** use the `satisfies` operator (TS 4.9+, assume 5.x) for config and constant objects. **DON'T** use `as Config` to assert types. **WHY**: `satisfies` validates the shape without widening literal types, so you keep narrow inferred values; `as` silences the compiler.

**DO** model variants as discriminated unions: `type Result<T> = { ok: true; value: T } | { ok: false; error: string }`. **DON'T** use boolean flags or `T | null | undefined` to express variants. **WHY**: the compiler narrows discriminated unions exhaustively in `switch`/`if` blocks.

**DO** use `zod` for runtime validation at every external boundary (API responses, env vars, form input). Derive types via `z.infer<typeof schema>`. **DON'T** trust `JSON.parse()` results as the schema you expect. **WHY**: TypeScript types vanish at runtime; schema validators are the only way to enforce them.

**DO** use branded types for domain identifiers: `type UserId = string & { readonly _brand: 'UserId' }`. **DON'T** pass raw `string` for IDs that should not be interchangeable. **WHY**: branded types prevent mixing `UserId` and `OrgId` at compile time with zero runtime cost.

**DO** prefer `function` declarations for module-level named exports. **DON'T** `export const fn = () => {}` for top-level named functions. **WHY**: function declarations hoist, give cleaner stack traces, and play better with `instanceof`/`name`.

**DO** use `unknown` for untyped inputs and narrow with type guards. **DON'T** use `any`. In `catch`, write `catch (e: unknown)` and check the type. **WHY**: `unknown` requires explicit narrowing; `any` silently disables the type system.

**DO** use `readonly` arrays (`readonly T[]`) and `readonly` properties for shared/immutable data. **DON'T** rely on convention to avoid mutation. **WHY**: `readonly` prevents accidental `push`/`splice`/assignment at compile time.

**DO** use ESM (`"type": "module"` in `package.json`) for new projects. **DON'T** mix CJS and ESM without an explicit interop strategy. **WHY**: ESM is the current standard; mixed-mode causes subtle bugs with named exports and dynamic `import()`.

**DO** annotate event handlers precisely: `(e: React.ChangeEvent<HTMLInputElement>) => void`. **DON'T** use `(e: any) => void`. **WHY**: precise event types catch wrong-element handler usage at compile time.

**DO** never use the non-null assertion `!` without an inline comment explaining why null is impossible here. **DON'T** sprinkle `!` to silence warnings. **WHY**: `!` is the type-system equivalent of "trust me bro" — usually it should be a runtime check.

**DO** keep modules tree-shake-friendly: import directly from leaf files. **DON'T** create barrel files (`index.ts` re-exporting everything) in large projects. **WHY**: barrel files inflate bundles and cause circular-dependency issues that defeat tree-shaking.

## Toolchain
- **Format + lint**: `biome` (single binary, replaces ESLint + Prettier for new projects)
- **Test runner**: `vitest` (NOT Jest for new TS projects)
- **Build**: `vite` (apps), `tsup`/`unbuild` (libraries), `tsc --noEmit` for typecheck
- **Package manager**: `pnpm` or `bun`; npm only for legacy
- **Runtime validation**: `zod` (or `valibot` if bundle size matters)

## Anti-Patterns to Reject
- `any` types in non-test code — use `unknown` and narrow.
- Non-null assertion `!` without an explaining comment.
- `as Config` to silence type errors — use `satisfies` instead.
- Barrel files (`index.ts` re-exports of every module) in large projects.
- `Function` and `Object` types — use specific signatures or `Record<string, unknown>`.
- `enum` (TS) — use `as const` object literals or string-literal unions instead; TS enums emit awkward JS.
- Mutating function parameters or shared arrays without explicit need.

## 2024-2025 Updates
- Biome replaced ESLint + Prettier for greenfield projects (mature in 2024).
- `satisfies` (TS 4.9+) is now the standard for config/constants — assume 5.x available.
- `using` / `Symbol.dispose` (TS 5.2+) for explicit resource management.
- Vitest dominates over Jest for new TS projects; ESM-native and `vi.*` API matches Jest.
- React 19 + Server Components shifted "default" patterns: see `react-idiomatic` skill.
