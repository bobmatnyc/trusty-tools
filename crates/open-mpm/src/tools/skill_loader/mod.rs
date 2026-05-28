//! `load_skill` and `list_skills` tools + filesystem-backed `SkillResolver`.
//!
//! Why: Agents need to pull in domain-specific Markdown guidance on demand.
//! A resolver abstraction lets tests use an in-memory map while production
//! walks a known directory hierarchy.
//! What: Split into focused submodules (#361):
//!   - `resolver`    — `FsSkillResolver` implements `SkillResolver`.
//!   - `loader_tool` — `SkillLoaderTool` wraps a resolver as `ToolExecutor`.
//!   - `list_tool`   — `SkillListTool` enumerates / tag-ranks skills.
//! Test: Place files in a tempdir, point a `FsSkillResolver` at it, verify
//! `resolve()` returns the content.

mod list_tool;
mod loader_tool;
mod resolver;

pub use list_tool::SkillListTool;
pub use loader_tool::SkillLoaderTool;
pub use resolver::FsSkillResolver;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::registry::SkillRegistry as TagSkillRegistry;
    use crate::tools::traits::{SkillResolver, ToolExecutor};
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;

    #[test]
    fn fs_resolver_reads_open_mpm_skills_md() {
        let tmp = tempdir();
        let skills_dir = tmp.join(".open-mpm").join("skills");
        fs::create_dir_all(&skills_dir).unwrap();
        let skill_path = skills_dir.join("foo.md");
        fs::write(&skill_path, "hello skill").unwrap();

        let resolver = FsSkillResolver::new(tmp.clone(), None);
        let got = resolver.resolve("foo").expect("should find skill");
        assert_eq!(got, "hello skill");
    }

    #[test]
    fn fs_resolver_reads_claude_skills_dir() {
        let tmp = tempdir();
        let skill_dir = tmp.join(".claude").join("skills").join("bar");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "bar content").unwrap();

        let resolver = FsSkillResolver::new(tmp.clone(), None);
        let got = resolver.resolve("bar").expect("should find skill");
        assert_eq!(got, "bar content");
    }

    #[test]
    fn fs_resolver_returns_none_for_unknown() {
        let tmp = tempdir();
        let resolver = FsSkillResolver::new(tmp, None);
        assert!(resolver.resolve("doesnotexist").is_none());
    }

    #[test]
    fn fs_resolver_list_enumerates_skills() {
        let tmp = tempdir();
        let skills_dir = tmp.join(".open-mpm").join("skills");
        fs::create_dir_all(&skills_dir).unwrap();
        fs::write(skills_dir.join("a.md"), "a").unwrap();
        fs::write(skills_dir.join("b.md"), "b").unwrap();
        let skill_dir = tmp.join(".claude").join("skills").join("c");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "c").unwrap();

        let resolver = FsSkillResolver::new(tmp.clone(), None);
        let list = resolver.list();
        assert!(list.contains(&"a".to_string()));
        assert!(list.contains(&"b".to_string()));
        assert!(list.contains(&"c".to_string()));
    }

    fn tempdir() -> PathBuf {
        let base =
            std::env::temp_dir().join(format!("open-mpm-skill-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[tokio::test]
    async fn list_skills_returns_by_tag() {
        let tmp = tempdir();
        fs::write(
            tmp.join("fastapi.md"),
            "---\nname: fastapi\ndescription: async routes\ntags: [python, fastapi]\n---\nbody\n",
        )
        .unwrap();
        fs::write(
            tmp.join("pytest.md"),
            "---\nname: pytest\ndescription: fixtures\ntags: [python, pytest]\n---\nbody\n",
        )
        .unwrap();
        fs::write(
            tmp.join("rust.md"),
            "---\nname: rust\ndescription: rust idioms\ntags: [rust]\n---\nbody\n",
        )
        .unwrap();

        let tag_reg = Arc::new(TagSkillRegistry::load(std::slice::from_ref(&tmp)));
        let resolver: Arc<dyn SkillResolver> = Arc::new(FsSkillResolver::new(tmp.clone(), None));
        let tool = SkillListTool::with_tag_registry(resolver, None, tag_reg);

        let result = tool.execute(json!({"tags": ["python"]})).await;
        let content = result.content();
        assert!(content.contains("\"fastapi\""));
        assert!(content.contains("\"pytest\""));
        assert!(
            !content.contains("\"rust\""),
            "rust has no python tag; should be filtered out: {content}"
        );

        // Multi-tag: overlap score should rank fastapi (2) above pytest (1).
        let result = tool.execute(json!({"tags": ["python", "fastapi"]})).await;
        let content = result.content();
        let fastapi_pos = content.find("\"fastapi\"").unwrap();
        let pytest_pos = content.find("\"pytest\"").unwrap();
        assert!(
            fastapi_pos < pytest_pos,
            "fastapi should rank first: {content}"
        );
        assert!(content.contains("\"match_score\":2"));
    }

    #[tokio::test]
    async fn list_skills_without_tags_returns_all_alphabetical() {
        let tmp = tempdir();
        fs::write(
            tmp.join("b.md"),
            "---\nname: b-skill\ndescription: d\ntags: [t]\n---\nbody\n",
        )
        .unwrap();
        fs::write(
            tmp.join("a.md"),
            "---\nname: a-skill\ndescription: d\ntags: [t]\n---\nbody\n",
        )
        .unwrap();
        let tag_reg = Arc::new(TagSkillRegistry::load(std::slice::from_ref(&tmp)));
        let resolver: Arc<dyn SkillResolver> = Arc::new(FsSkillResolver::new(tmp.clone(), None));
        let tool = SkillListTool::with_tag_registry(resolver, None, tag_reg);

        let result = tool.execute(json!({})).await;
        let content = result.content();
        let a_pos = content.find("\"a-skill\"").expect("a-skill in output");
        let b_pos = content.find("\"b-skill\"").expect("b-skill in output");
        assert!(a_pos < b_pos, "alphabetical order expected: {content}");
    }
}
