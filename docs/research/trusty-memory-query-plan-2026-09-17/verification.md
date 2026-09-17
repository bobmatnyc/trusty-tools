# Verification and scope

All checks below refer to the new query-plan experiment. No production installation or real-user memory retrieval was performed.

| Check | Evidence |
|---|---|
| Focused regression suite | [19 passed](evidence/pytest.log) |
| Strict type checking | [Seven modules clean](evidence/mypy.log) |
| Independent initial review | [Two HIGH and one MEDIUM finding](evidence/critic-initial.md), all repaired before ranking |
| Independent resolution review | [APPROVE](evidence/critic-final.md), exclusion/scope/clocks/candidate-loss probes passed |
| Security review | [Zero HIGH/CRITICAL](evidence/security.md); additional LOW literal-marker input crash fixed |
| Final guarded suite | [EXIT=0](evidence/security-guard.log), Python network/model constructors/official fixture access blocked during tests |
| Raw fixture audit | [Independent source, gold, span and eligibility checks](evidence/fixture-audit.json) |
| Official fixed-arm run | [Exit 0](evidence/evaluation.log), all tune and heldout arms complete |
| Independent packet/source audit | [Recomputed raw semantic metrics and temporal provenance](evidence/results-independent.md) |
| Independent numerical audit | [Aggregates, candidate equality, counts and budgets](evidence/results-audit.log) |

The reviewer approved the three initial fixes with 17 tests passing. The later LOW reserved-marker fix added two cases and was verified by the security guard, bringing the final suite to 19. No official rankings preceded these fixes. The [initial fixture freeze](evidence/fixture-freeze.sha256) predates the reviewed scope-context interface amendment; the final experiment manifest includes that amendment and all final code. Fixture bytes and grammar were not changed to match results.

The inherited helper IPC deadline and transitive dependency-lock LOW observations remain documented in the security review. Guards cover Python network operations and model construction, not native syscall-level egress. Experimental checkpoints and manifests are trusted local inputs. No current dependency-advisory clearance is claimed.

The reused dream-cycle maintenance has bounded source batches but retains whole-map copying/validation work. Passing logical incremental-rebuild equivalence does not establish production CPU/RAM bounds or crash-durable native index publication. These remain integration conditions for #8246.
