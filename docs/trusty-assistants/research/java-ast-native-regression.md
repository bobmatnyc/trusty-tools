# Java AST-Native Regression — Root Cause Analysis

**Date**: 2026-05-08  
**Related**: Issue #358 (multi-language eval), Issue #362 (EmitStrategy)

## Executive Summary

Java regressed +44.6% wall-clock and lost 50% of tests when run with AST-native mode. Root cause: `tree-sitter-java` is not present in the project. All six AST tools hard-fail on `.java` files with "unsupported file extension", forcing the engineer agent into wasted LLM roundtrips before falling back to full-file writes.

## Evidence

### No tree-sitter-java dependency

- `Cargo.toml` and `crates/symgraph/Cargo.toml` only list: `tree-sitter-rust`, `tree-sitter-python`, `tree-sitter-javascript`, `tree-sitter-go`
- `crates/symgraph/src/parser.rs`: `Language` enum has Rust/Python/JavaScript/Go only
- `detect_language()` returns `None` for `.java` extensions

### Hard errors on every AST tool call

- `crates/symgraph/src/editor.rs` lines 52–88: all tools call `detect_language(file).with_context(|| format!("unsupported file extension: {}", file.display()))?`
- `src/tools/ast_tools.rs` line 395: `validate_syntax` also errors on Java
- Tool descriptions list only `.rs/.py/.js/.go` — Java explicitly absent

### --compare flag bypasses per-phase overrides

- `src/main.rs` sets `ast_native_override = true` globally for the AST-native run
- `prescriptive.json` code phase has no explicit `"ast_native"` key → inherits global `true`
- Research/plan phases already have `"ast_native": true` but only produce JSON/markdown (no `.java` files written there)

### Observed effect

| Metric | Traditional | AST-native | Change |
|---|---|---|---|
| Wall-clock | 649,562ms | 939,476ms | +44.6% |
| Tests | 16 | 8 | -50% |
| Output files | 15 | 19 | +4 extra docs |

The extra 4 docs in AST-native output (API_SUMMARY.md, DOCUMENTATION.md, TESTING.md) indicate the agent spent leftover token budget on documentation after tool failures — a clear signal the code phase went off-script.

## Root Cause Analysis

### Phase 1: AST-native mode activation

The `--compare` flag in `src/main.rs` activates AST-native mode by setting a global process-wide override:

```rust
if args.compare {
    ast_native_override = true;
}
```

This override applies to all workflow phases uniformly, unless explicitly overridden per-phase.

### Phase 2: Tool invocation on Java files

When the engineer agent attempts to write Java code (`.java` files), it invokes AST tools:

1. `edit_in_file()` calls `detect_language(file)?`
2. `detect_language()` returns `None` for `.java` extension
3. Tool chain errors out with "unsupported file extension: *.java"

This happens for every tool in the AST arsenal:
- `read_lines()`
- `edit_in_file()`
- `search_in_file()`
- `list_symbols()`
- `validate_syntax()`
- `find_references()`

### Phase 3: Failure response cascade

When tools fail, the engineer agent:

1. Attempts write via AST tools → error
2. Falls back to full-file write (longer operation)
3. Consumes more tokens per operation
4. Fewer code iterations fit in token budget
5. Remaining token budget spills into documentation generation

This explains the extra 4 markdown files in the AST-native run.

### Phase 4: Test impact

With fewer code iterations:

- Less time debugging and refining code
- Tests written late or skipped due to token budget
- 8 tests vs. 16 tests = 50% reduction

## Fix Applied

**Option A (immediate)**: Added `"ast_native": false` to the `code` and `qa` phases in `.open-mpm/workflows/prescriptive.json`. The per-phase `AstNativeGuard` in `src/workflow/engine.rs` takes precedence over the process-wide override.

```json
"code": {
  "ast_native": false,
  ...
},
"qa": {
  "ast_native": false,
  ...
}
```

This allows:
- Research/plan phases: benefit from AST optimization (JSON/markdown only)
- Code/qa phases: traditional tool access (handles Java gracefully)

**Option B (planned, issue opened)**: Add `tree-sitter-java` grammar support

Implement:
- `Language::Java` enum variant in `crates/symgraph/src/parser.rs`
- `detect_language()` arm for `.java` extension
- Symbol kind mappings:
  - `method_declaration` → Function
  - `class_declaration` → Class/Struct
  - `interface_declaration` → Trait
  - `import_declaration` → Import
  - `package_declaration` → Module

This would eliminate Java as a special case and restore AST-native benefits for code phases.

## Lessons Learned

1. **AST-native mode should only be enabled for languages with confirmed grammar support** — Missing grammar support causes silent tool failures, not graceful degradation.

2. **`--compare` should detect unsupported languages and warn/skip** — The comparison would be more meaningful if the AST-native run succeeded on the same file types.

3. **Per-phase `"ast_native": false` is the correct defensive default** — Code/qa phases generate production code and require high reliability. Let research/plan phases benefit from AST savings; protect code generation quality.

4. **Tool error handling should surface unsupported languages early** — The 44.6% regression was invisible until comparing full workflows. Per-tool errors should aggregate and fail fast.

## Post-Fix Verification

**Date**: 2026-05-08  
**Fix Applied**: `tree-sitter-java = "0.23"` added to Cargo.toml; `ast_native: false` removed from `.open-mpm/workflows/prescriptive.json` code and qa phases.

**Post-fix results** (run timestamp 2026-05-08T16:16:11Z):

| Metric | Before fix | After fix | Status |
|---|---|---|---|
| Wall-clock Traditional | 649,562ms | 631,760ms | ✓ Improved |
| Wall-clock AST-native | 939,476ms | 585,909ms | ✓ Fixed |
| Delta direction | **Regression** (+44.6%) | **Improvement** (-7.3%) | ✓ Resolved |
| Tests Traditional | 16 | 17 | ✓ Improved |
| Tests AST-native | 8 (-50%) | 17 (parity) | ✓ Fixed |
| Syntax valid | YES/YES | YES/YES | ✓ Maintained |

**Conclusion**: Regression fully resolved. The addition of `tree-sitter-java` grammar eliminated all hard-fail tool errors on `.java` files. AST-native now shows a modest **-7.3% improvement**, consistent with TypeScript results and supporting the recommendation to enable AST-native as the default for Java code generation.

## Timeline

- **2026-05-07**: Regression discovered during `--compare` workflow test
- **2026-05-08**: Root cause identified; Option A (defensive guard) applied
- **2026-05-08**: This analysis documented; Option B opened as future issue
- **2026-05-08**: Option B implemented: `tree-sitter-java` added to Cargo.toml; defensive guard removed
- **2026-05-08**: Post-fix evaluation confirms regression fully resolved

## Related Issues

- **#358**: Multi-language evaluation pipeline
- **#362**: EmitStrategy refactor (may simplify per-phase overrides)

## Next Steps

1. ~~Monitor workflow performance after per-phase `ast_native` fix~~ — Complete
2. ~~Prioritize `tree-sitter-java` implementation for Option B~~ — Complete
3. Update multi-language eval doc with post-fix Java results
4. Consider language support matrix in agent validation
5. Add pre-flight checks for language grammar availability
