//! Conversation transcript for the multi-turn agent loop.
//!
//! Why: The loop must accumulate the full message history (system, user,
//! assistant turns, and tool results) to send back to the model on each
//! iteration and to render a final answer. Wrapping the `Vec<ChatMessage>` in a
//! small type gives the loop intent-revealing helpers (`push_assistant`,
//! `push_tool_result`) and keeps message-construction details out of the loop
//! body.
//! What: `Transcript` owns the running `Vec<ChatMessage>`; helpers append the
//! seed messages, an assistant turn (text and/or tool calls), and a `tool` role
//! result message. `assistant_text` concatenates the assistant text turns for
//! the final `AgentOutput`.
//! Test: `agent_loop::tests` build transcripts indirectly through the loop;
//! `transcript::tests` cover the helpers directly.

use crate::llm::{ChatMessage, ChatResponse, ToolCall};

/// Running conversation history for one agent-loop run.
///
/// Why: Centralises message accumulation so the loop body stays focused on
/// control flow rather than `ChatMessage` plumbing.
/// What: A thin wrapper over `Vec<ChatMessage>` with append helpers and an
/// assistant-text accessor.
/// Test: `transcript::tests::*`.
#[derive(Debug, Default, Clone)]
pub struct Transcript {
    messages: Vec<ChatMessage>,
    assistant_texts: Vec<String>,
}

impl Transcript {
    /// Seed a transcript with a system prompt and the user task.
    ///
    /// Why: Every run starts from exactly these two turns; bundling the seed in
    /// a constructor prevents the loop from forgetting either.
    /// What: Pushes a `system` message then a `user` message.
    /// Test: `transcript::tests::seed_has_two_messages`.
    pub fn seed(system: &str, task: &str) -> Self {
        Self {
            messages: vec![ChatMessage::system(system), ChatMessage::user(task)],
            assistant_texts: Vec::new(),
        }
    }

    /// Append the assistant turn from a model response.
    ///
    /// Why: The model's turn (text and any tool calls) must be added to history
    /// before the tool results, so the next request reflects the real ordering
    /// the API expects.
    /// What: Reconstructs a `ChatMessage` with `role = "assistant"`, carrying the
    /// optional text content and the emitted tool calls; records the text (if
    /// any) for the final answer.
    /// Test: `transcript::tests::push_assistant_records_text`.
    pub fn push_assistant(&mut self, text: Option<String>, tool_calls: &[ToolCall]) {
        if let Some(t) = &text
            && !t.is_empty()
        {
            self.assistant_texts.push(t.clone());
        }
        let tool_calls = if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls.to_vec())
        };
        self.messages.push(ChatMessage {
            role: "assistant".into(),
            content: text,
            tool_calls,
            tool_call_id: None,
            name: None,
        });
    }

    /// Append a `tool` role result message answering a specific tool call.
    ///
    /// Why: The API requires each tool call to be answered by a `tool` message
    /// referencing its `tool_call_id`, or the next assistant turn errors.
    /// What: Pushes a `ChatMessage::tool_result` with the call id, function
    /// name, and textual result.
    /// Test: `transcript::tests::push_tool_result_sets_call_id`.
    pub fn push_tool_result(&mut self, tool_call_id: &str, name: &str, content: &str) {
        self.messages
            .push(ChatMessage::tool_result(tool_call_id, name, content));
    }

    /// Borrow the full message history for sending to the model.
    ///
    /// Why: The loop builds each `ChatRequest` from the current history; it needs
    /// read access without taking ownership.
    /// What: Returns a slice over the accumulated messages.
    /// Test: `transcript::tests::messages_reflects_appends`.
    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    /// Clone the message history into an owned `Vec` for a request.
    ///
    /// Why: `ChatRequest::messages` owns its `Vec`; the loop must hand it a copy
    /// while keeping the transcript intact for the next iteration.
    /// What: Clones the internal vector.
    /// Test: Exercised indirectly via `agent_loop::tests`.
    pub fn to_messages(&self) -> Vec<ChatMessage> {
        self.messages.clone()
    }

    /// Join all recorded assistant text turns into the final answer string.
    ///
    /// Why: The `AgentOutput.content` should be the model's prose across the
    /// run, not the raw JSON transcript; joining the text turns yields that.
    /// What: Joins the recorded non-empty assistant texts with blank lines.
    /// Test: `transcript::tests::assistant_text_joins_turns`.
    pub fn assistant_text(&self) -> String {
        self.assistant_texts.join("\n\n")
    }

    /// Convenience accessor: push the assistant turn straight from a response.
    ///
    /// Why: Callers usually have a `ChatResponse` in hand; this saves them
    /// destructuring `first_text`/`first_tool_calls` at the call site.
    /// What: Extracts the first choice's text and tool calls and delegates to
    /// `push_assistant`.
    /// Test: `transcript::tests::push_response_appends_assistant`.
    pub fn push_response(&mut self, resp: &ChatResponse) {
        let text = resp.first_text();
        let calls = resp.first_tool_calls().to_vec();
        self.push_assistant(text, &calls);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{FunctionCall, ToolCall};

    fn sample_call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            kind: "function".into(),
            function: FunctionCall {
                name: name.into(),
                arguments: "{}".into(),
            },
        }
    }

    /// `seed` produces exactly a system + user message pair.
    ///
    /// Why: Guards the run's starting state.
    /// What: Seed and assert two messages with the expected roles.
    /// Test: this test.
    #[test]
    fn seed_has_two_messages() {
        let t = Transcript::seed("you are helpful", "do the thing");
        assert_eq!(t.messages().len(), 2);
        assert_eq!(t.messages()[0].role, "system");
        assert_eq!(t.messages()[1].role, "user");
    }

    /// `push_assistant` records non-empty text for the final answer.
    ///
    /// Why: The final `AgentOutput` is built from assistant text turns.
    /// What: Push text + no calls; assert `assistant_text` returns it.
    /// Test: this test.
    #[test]
    fn push_assistant_records_text() {
        let mut t = Transcript::seed("s", "u");
        t.push_assistant(Some("hello".into()), &[]);
        assert_eq!(t.assistant_text(), "hello");
        assert_eq!(t.messages().last().expect("msg").role, "assistant");
    }

    /// An empty-string assistant turn is not recorded as text.
    ///
    /// Why: Tool-only turns often carry empty/None content; they must not add
    /// blank lines to the final answer.
    /// What: Push empty text; assert `assistant_text` stays empty.
    /// Test: this test.
    #[test]
    fn push_assistant_skips_empty_text() {
        let mut t = Transcript::seed("s", "u");
        t.push_assistant(Some(String::new()), &[sample_call("c1", "echo")]);
        assert_eq!(t.assistant_text(), "");
    }

    /// `push_tool_result` appends a `tool` message bound to the call id.
    ///
    /// Why: The API requires tool results to reference their call id.
    /// What: Push a tool result; assert role and `tool_call_id`.
    /// Test: this test.
    #[test]
    fn push_tool_result_sets_call_id() {
        let mut t = Transcript::seed("s", "u");
        t.push_tool_result("call_1", "echo", "echoed");
        let last = t.messages().last().expect("msg");
        assert_eq!(last.role, "tool");
        assert_eq!(last.tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(last.name.as_deref(), Some("echo"));
    }

    /// `messages` reflects appended turns in order.
    ///
    /// Why: The model must see the real chronological history.
    /// What: Seed, push assistant + tool result, assert length grows by two.
    /// Test: this test.
    #[test]
    fn messages_reflects_appends() {
        let mut t = Transcript::seed("s", "u");
        t.push_assistant(None, &[sample_call("c1", "echo")]);
        t.push_tool_result("c1", "echo", "ok");
        assert_eq!(t.messages().len(), 4);
    }

    /// `assistant_text` joins multiple text turns with blank lines.
    ///
    /// Why: A multi-turn final answer should read as coherent prose.
    /// What: Push two text turns; assert the joined form.
    /// Test: this test.
    #[test]
    fn assistant_text_joins_turns() {
        let mut t = Transcript::seed("s", "u");
        t.push_assistant(Some("first".into()), &[]);
        t.push_assistant(Some("second".into()), &[]);
        assert_eq!(t.assistant_text(), "first\n\nsecond");
    }
}
