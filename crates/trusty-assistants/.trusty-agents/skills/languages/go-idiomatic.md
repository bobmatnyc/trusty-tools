---
name: go-idiomatic
tags: [language, idioms, go, golang]
summary: Idiomatic Go coding guidelines — 2024-2025
---

# Idiomatic Go — 2024-2025

## Core Philosophy
Idiomatic Go in 2024-2025 means small interfaces, explicit error handling, table-driven tests, `context.Context` everywhere I/O happens, `slog` structured logging, and judicious generics. The community optimizes for boring, readable code: "share memory by communicating", interfaces accepted / structs returned, and zero cleverness in production paths.

## Idioms: DO / DON'T / WHY

**DO** keep interfaces tiny — 1 to 3 methods — and define them where they are consumed, not where they are implemented. **DON'T** define large interfaces upfront or in the producer package. **WHY**: small interfaces compose cleanly; large interfaces are hard to satisfy and almost always indicate the wrong abstraction.

**DO** handle errors immediately at the call site: `if err != nil { return fmt.Errorf("loading config: %w", err) }`. **DON'T** defer error handling or accumulate errors silently. **WHY**: Go's error model is explicit by design; deferred handling obscures the source and hides recoverable failures.

**DO** wrap errors with `fmt.Errorf("...: %w", err)` and inspect them with `errors.Is` / `errors.As`. **DON'T** compare errors with `==` (it misses wrapped errors), and don't drop context with bare `return err` through several layers. **WHY**: `%w` preserves the error chain; `errors.Is`/`errors.As` traverse the chain.

**DO** use the stdlib `slog` (Go 1.21+) for structured logging. **DON'T** add `logrus`, `zap`, or `zerolog` to new projects. **WHY**: `slog` is in the standard library, supports structured key/value attributes, and works with all observability backends through handlers.

**DO** use the stdlib `slices` and `maps` packages (Go 1.21+) for common operations: `slices.Contains`, `slices.Sort`, `maps.Keys`. **DON'T** roll your own utility functions or import third-party generic helper libraries. **WHY**: stdlib generics cover the 90% case with no dependency cost.

**DO** pass `context.Context` as the first parameter to every function that does I/O, blocks, or has a deadline: `func Query(ctx context.Context, q string) (...)`. **DON'T** store context in structs, and don't use `context.Background()` deep in call chains. **WHY**: context propagates cancellation and deadlines; struct-stored context defeats the chain.

**DO** write table-driven tests using a slice of anonymous structs and `t.Run(tt.name, ...)`. **DON'T** copy-paste test functions per case. **WHY**: table-driven tests scale by data, run subtests in parallel, and provide named failure output.

**DO** use range-over-integers (Go 1.22+): `for i := range n { ... }`. **DON'T** write `for i := 0; i < n; i++` for simple counted loops. **WHY**: shorter, harder to off-by-one, and standard since 1.22.

**DO** name packages by their function (`http`, `json`, `auth`), and let identifiers read naturally with the package name (`http.Get`, not `http.HTTPGet`). **DON'T** create generic packages named `util`, `common`, `helpers`, `misc`. **WHY**: package names are part of the API; generic dumping-ground packages lose meaning over time.

**DO** "share memory by communicating" — use channels for goroutine coordination. **DON'T** share mutable state across goroutines without a mutex or channel. **WHY**: this is Go's concurrency model; violating it produces data races that the race detector will catch but the design problem won't go away.

**DO** use generics (Go 1.18+) only when the abstraction is genuinely type-parametric (`Map`, `Filter`, type-safe sets). **DON'T** use generics to avoid two near-duplicate concrete implementations — interfaces almost always suffice. **WHY**: generics add cognitive load; community consensus is "interfaces first, generics rarely".

**DO** declare with `var` when you want the zero value (`var mu sync.Mutex`), and `:=` when you initialize. Use `&T{}` for pointer initialization. **DON'T** use `new(T)`. **WHY**: zero values are valid for `sync.Mutex`, channels, slices, and maps; `&T{}` makes the type explicit; `new(T)` is an older idiom rarely seen in modern Go.

## Toolchain
- **Format**: `gofmt` / `goimports` (no debate)
- **Lint**: `golangci-lint` with at minimum `errcheck`, `staticcheck`, `govet`, `unused`, `gosimple`
- **Test runner**: `go test ./...`; coverage with `go test -cover`
- **Build**: `go build`, `go install`; modules via `go.mod` (no GOPATH for new code)
- **Version**: target Go 1.22+ for `range` over int and recent stdlib

## Anti-Patterns to Reject
- Global state and package-level mutable variables — pass dependencies explicitly.
- `init()` functions that do real work — they hurt testability and ordering.
- Naked returns in functions longer than 5 lines — they hide what's being returned.
- Generic package names: `util`, `common`, `helpers`.
- Catching errors with `_ = thing()` to silence them — handle or propagate.
- Storing `context.Context` in a struct field — pass it as a parameter.
- Returning concrete types where consumers only need an interface — but only define the interface in the consumer.

## 2024-2025 Updates
- `slog` (Go 1.21) is the standard structured logger; `logrus`/`zap`/`zerolog` are legacy for new projects.
- `slices`, `maps`, and `cmp` packages (Go 1.21) made generic helper libraries unnecessary.
- Range-over-integers (`for i := range n`) is Go 1.22+ — use it.
- Generics matured; community consensus is "use sparingly, interfaces first".
- `errors.Join` (Go 1.20+) for accumulating multiple errors before returning.
