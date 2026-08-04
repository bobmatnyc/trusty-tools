Performance

- bundled agents no longer carry boilerplate `skills:` frontmatter, cutting the skill bodies the harness renders into every dispatch by 78% (2,602,763 → 560,654 bytes across the 37-agent roster) (closes [#4642](https://github.com/bobmatnyc/trusty-tools/issues/4642))
  - worst case `qa` 84,343 → 21,170 bytes (~21,085 → ~5,292 tokens); `research` 55,894 → 0
  - an omitted skill is unchanged on disk and still invokable on demand via the Skill tool
  - a new regression gate caps any bundled agent's resident skill bodies at 24,000 bytes (34,000 for the rust family, which keeps `rust-build-performance` per the standing 2026-07-17 directive)
