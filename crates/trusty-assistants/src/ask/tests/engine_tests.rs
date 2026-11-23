//! The ask engine end to end, offline (#11650).
//!
//! Test: this module IS the test surface.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::json;

use super::{Scripted, empty_servers, instance, text, tool_call};
use crate::agents::FALLBACK_MODEL;
use crate::ask::{AskEngine, AskError, OKG_SEARCH_TOOL_NAME, TurnLog};
use crate::assistants::{AssistantError, AssistantHome};

fn engine(home: &AssistantHome, llm: &Arc<Scripted>, tmp: &tempfile::TempDir) -> AskEngine {
    AskEngine::new(home.clone(), llm.clone()).with_mcp_config(empty_servers(tmp))
}

fn add_ask_table(home: &AssistantHome, table: &str) {
    let mut body = std::fs::read_to_string(home.config_path()).unwrap();
    body.push_str(table);
    std::fs::write(home.config_path(), body).unwrap();
}

#[tokio::test]
async fn a_second_ask_sees_the_first_turn() {
    let tmp = tempfile::tempdir().unwrap();
    let home = instance(&tmp);
    let first_q = "Correction: my name is Robert, not Rob.";
    let first_a = "Noted. I will call you Robert.";

    let llm = Arc::new(Scripted::new(vec![text(first_a)]));
    let answer = engine(&home, &llm, &tmp).ask(first_q).await.unwrap();
    assert_eq!(answer, first_a);

    let llm = Arc::new(Scripted::new(vec![text("Your name is Robert.")]));
    let answer = engine(&home, &llm, &tmp)
        .ask("What is my name?")
        .await
        .unwrap();
    assert_eq!(answer, "Your name is Robert.");

    let request = &llm.requests()[0];
    let roles: Vec<&str> = request.messages.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(roles, ["system", "user", "assistant", "user"]);
    let content = |i: usize| request.messages[i].content.clone().unwrap_or_default();
    let system = content(0);
    assert!(system.contains("INSTRUCTIONS-MARKER"), "{system}");
    assert!(system.contains("okg://izzie"), "{system}");
    assert!(
        system.contains(&home.okg_dir().display().to_string()),
        "{system}"
    );
    assert!(system.contains(OKG_SEARCH_TOOL_NAME), "{system}");
    assert_eq!(content(1), first_q);
    assert_eq!(content(2), first_a);
    assert_eq!(content(3), "What is my name?");

    let turns = TurnLog::for_home(&home).read().unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].question, first_q);
    assert_eq!(turns[1].answer, "Your name is Robert.");
}

#[tokio::test]
async fn cancel_stops_at_the_next_turn_boundary() {
    let tmp = tempfile::tempdir().unwrap();
    let home = instance(&tmp);
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancel);
    // Turn 1 asks for a tool and sets the flag while it is in flight.
    let llm = Arc::new(
        Scripted::new(vec![
            tool_call(OKG_SEARCH_TOOL_NAME, json!({ "query": "x" })),
            text("never reached"),
        ])
        .on_call(move || flag.store(true, Ordering::Relaxed)),
    );
    let result = engine(&home, &llm, &tmp)
        .with_cancel_flag(cancel)
        .ask("Find x")
        .await;
    assert!(matches!(result, Err(AskError::Cancelled)), "{result:?}");
    assert_eq!(llm.requests().len(), 1, "no turn after the boundary");
    assert!(!TurnLog::for_home(&home).path().exists());
}

#[tokio::test]
async fn an_instructions_error_stops_the_ask() {
    let tmp = tempfile::tempdir().unwrap();
    let home = instance(&tmp);
    let llm = Arc::new(Scripted::new(vec![text("unused")]));

    std::fs::write(home.instructions_path(), "  \n").unwrap();
    let result = engine(&home, &llm, &tmp).ask("hi").await;
    assert!(
        matches!(
            result,
            Err(AskError::Instructions(
                AssistantError::InstructionsEmpty { .. }
            ))
        ),
        "{result:?}"
    );

    std::fs::remove_file(home.instructions_path()).unwrap();
    let result = engine(&home, &llm, &tmp).ask("hi").await;
    assert!(
        matches!(
            result,
            Err(AskError::Instructions(
                AssistantError::InstructionsMissing { .. }
            ))
        ),
        "{result:?}"
    );
    assert!(
        llm.requests().is_empty(),
        "no model call without instructions"
    );
}

#[tokio::test]
async fn a_failed_ask_never_advances_the_log() {
    let tmp = tempfile::tempdir().unwrap();
    let home = instance(&tmp);
    let log = TurnLog::for_home(&home);
    let llm = Arc::new(Scripted::new(vec![text("first answer")]));
    engine(&home, &llm, &tmp).ask("first").await.unwrap();
    let before = std::fs::read(log.path()).unwrap();

    // The script runs out: an inference error.
    let llm = Arc::new(Scripted::new(vec![]));
    let result = engine(&home, &llm, &tmp).ask("q").await;
    assert!(matches!(result, Err(AskError::Loop(_))), "{result:?}");

    // Two empty turns: no answer text.
    let llm = Arc::new(Scripted::new(vec![text(""), text("")]));
    let result = engine(&home, &llm, &tmp).ask("q").await;
    assert!(matches!(result, Err(AskError::EmptyAnswer)), "{result:?}");

    let result = engine(&home, &llm, &tmp).ask("   ").await;
    assert!(matches!(result, Err(AskError::EmptyQuestion)), "{result:?}");

    // One turn allowed, spent on a tool call.
    add_ask_table(&home, "[ask]\nmax_turns = 1\n");
    let llm = Arc::new(Scripted::new(vec![tool_call(
        OKG_SEARCH_TOOL_NAME,
        json!({ "query": "x" }),
    )]));
    let result = engine(&home, &llm, &tmp).ask("q").await;
    assert!(
        matches!(result, Err(AskError::TurnCapExceeded { max_turns: 1 })),
        "{result:?}"
    );
    assert_eq!(std::fs::read(log.path()).unwrap(), before);

    // A corrupt log stops the ask before any model call.
    let mut corrupt = before.clone();
    corrupt.extend_from_slice(b"{not json\n");
    std::fs::write(log.path(), &corrupt).unwrap();
    let llm = Arc::new(Scripted::new(vec![text("unused")]));
    let result = engine(&home, &llm, &tmp).ask("q").await;
    assert!(
        matches!(result, Err(AskError::CorruptTurnLog { line: 2, .. })),
        "{result:?}"
    );
    assert!(llm.requests().is_empty());
    assert_eq!(std::fs::read(log.path()).unwrap(), corrupt);
}

#[tokio::test]
async fn an_empty_model_is_sent_as_the_default() {
    let tmp = tempfile::tempdir().unwrap();
    let home = instance(&tmp);
    let seed = std::fs::read_to_string(home.config_path()).unwrap();

    for (table, expected) in [
        ("[ask]\nmodel = \"\"\n", FALLBACK_MODEL),
        ("", FALLBACK_MODEL),
        ("[ask]\nmodel = \"openai/gpt-4o\"\n", "openai/gpt-4o"),
    ] {
        std::fs::write(home.config_path(), format!("{seed}{table}")).unwrap();
        let llm = Arc::new(Scripted::new(vec![text("ok")]));
        engine(&home, &llm, &tmp).ask("hi").await.unwrap();
        assert_eq!(llm.requests()[0].model, expected, "{table:?}");
    }
}
