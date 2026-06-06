---
name: java-idiomatic
tags: [language, idioms, java]
summary: Idiomatic Java coding guidelines — 2024-2025 (Java 21+)
---

# Idiomatic Modern Java — 2024-2025

## Core Philosophy
Idiomatic Java in 2024-2025 means Java 21 LTS, records over POJOs, sealed interfaces with exhaustive pattern matching, switch expressions, virtual threads for I/O concurrency, and AssertJ-flavored JUnit 5 tests. The community optimizes for expressive value-types, exhaustive sum types checked by the compiler, and platform threads only when explicitly required.

## Idioms: DO / DON'T / WHY

**DO** model immutable data with `record` (Java 16+): `public record Point(int x, int y) {}`. **DON'T** hand-write POJO classes with explicit constructor / getters / `equals` / `hashCode`. **WHY**: records auto-generate all the boilerplate, enforce immutability, and signal intent.

**DO** model sum types with `sealed interface` + `permits` (Java 17+) and exhaustive switch pattern matching. **DON'T** use abstract classes with `instanceof` chains. **WHY**: sealed hierarchies let the compiler check exhaustiveness in `switch`, eliminating "what if a new variant is added" bugs.

**DO** use switch expressions with arrow syntax: `String label = switch (status) { case ACTIVE -> "Active"; case INACTIVE -> "Inactive"; };`. **DON'T** write `switch` statements with `break` and fallthrough. **WHY**: switch expressions return values, are exhaustiveness-checked, and eliminate fallthrough bugs.

**DO** use text blocks (`"""..."""`) for multi-line strings (JSON, SQL, HTML). **DON'T** concatenate with `+` and `\n`. **WHY**: text blocks handle indentation stripping, are valid embedded JSON/SQL, and are far more readable.

**DO** use virtual threads (`Executors.newVirtualThreadPerTaskExecutor()`, `Thread.ofVirtual().start(...)`) for I/O-bound concurrency on Java 21+. **DON'T** create platform thread pools for high-concurrency I/O. **WHY**: virtual threads cost ~kilobytes; platform threads cost ~1MB stack each. A million virtual threads is fine.

**DO** return `Optional<T>` from public APIs that may legitimately produce no value. **DON'T** return `null` from public APIs, and don't use `Optional` as a field or method-parameter type. **WHY**: `Optional` forces callers to handle absence; field `Optional` wastes memory and breaks serialization.

**DO** stream-and-collect with `Collectors.toUnmodifiableList()` (or `Stream.toList()` on Java 16+). **DON'T** stream into a mutable `ArrayList` you then expose. **WHY**: unmodifiable collections prevent unintended mutation by callers.

**DO** chain test assertions with AssertJ: `assertThat(result).isNotNull().extracting("field").isEqualTo(expected)`. **DON'T** rely on JUnit 5's `assertEquals` for complex object comparisons. **WHY**: AssertJ's fluent API gives drastically better failure messages and reads top-to-bottom.

**DO** use `@ParameterizedTest` with `@MethodSource` / `@CsvSource` for data-driven cases. **DON'T** copy-paste test methods that differ only in input. **WHY**: parameterized tests scale linearly in data, not in code.

**DO** prefer method references and lambdas over anonymous inner classes: `list.forEach(System.out::println)`. **DON'T** write four-line anonymous classes when a method reference suffices. **WHY**: method references signal intent more clearly and produce smaller bytecode.

**DO** use `record` patterns and `var` (Java 10+) for local variables when the right-hand side makes the type obvious. **DON'T** use `var` for fields, parameters, or return types — those are part of public contracts. **WHY**: local `var` reduces noise; public type annotations are documentation.

**DO** treat checked exceptions as a last resort. Wrap them in unchecked exceptions at API boundaries. **DON'T** propagate `IOException` / `SQLException` through multi-layer service APIs. **WHY**: checked exceptions force every caller to handle or rethrow, which leaks implementation details upward.

## Toolchain
- **Java version**: 21 LTS (target for new services)
- **Build**: Gradle 8 (Kotlin DSL) or Maven 3.9+
- **Test framework**: JUnit 5 + AssertJ + Mockito 5
- **Static analysis**: Error Prone, SpotBugs, PMD
- **Format**: google-java-format or Spotless
- **Framework default**: Spring Boot 3.x (Jakarta EE 9+, virtual-thread-aware, GraalVM native-image-ready)

## Anti-Patterns to Reject
- Raw types (`List` without `<T>`) — always parameterize generics.
- Returning `null` from public APIs — return `Optional<T>` or throw.
- Checked exceptions across service-layer / API boundaries — wrap and rethrow unchecked.
- Hand-written getters/setters/equals/hashCode for value classes — use `record`.
- `instanceof` chains — use sealed interfaces + pattern matching.
- Platform thread pools for I/O concurrency on Java 21+ — use virtual threads.
- Field-level `Optional<T>` — use the bare type and document nullability.

## 2024-2025 Updates
- Java 21 LTS (released 2023) is the target for new services; virtual threads are stable.
- Pattern matching for `switch` is stable (JEP 441, Java 21) — use it for exhaustive sum-type dispatch.
- Spring Boot 3.x defaults to Jakarta EE namespaces and is virtual-thread aware.
- Unnamed classes + instance `main` (JEP 463/477) preview for scripting — not stable as of 2024.
- String Templates (JEP 430) remained in preview through 2024 — wait for stabilization before adopting.
