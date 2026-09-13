Fixed

- Roster agents composed by tcode no longer carry the raw `{{TM_SKILLS}}` placeholder. tcode embeds the three skill files `BASE-AGENT.md` points at, writes them to `<project>/.trusty-code/skill-refs` for the project roster deploy or `~/.trusty-code/skill-refs` for in-process composes, and resolves the placeholder to that directory. Neither directory is a skill-discovery path, so the copies never shadow the embedded skill catalog (#7727).
