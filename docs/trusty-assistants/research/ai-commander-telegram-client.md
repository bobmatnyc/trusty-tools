# ai-commander Telegram Client — Architecture Research

**Date**: 2026-04-30
**Source**: `~/Projects/ai-commander/crates/commander-telegram/`
**Purpose**: Port patterns to open-mpm Rust Telegram integration

---

## Summary

ai-commander's Telegram integration is a **pure Rust implementation** using
**teloxide 0.17** (not a Python project). The crate `commander-telegram` is a
standalone binary that runs as a long-lived process alongside the main API server.

---

## 1. Library Choice

| | Detail |
|---|---|
| **Crate** | `teloxide = { version = "0.17", features = ["webhooks-axum", "macros"] }` |
| **Async runtime** | `tokio` (full features) |
| **HTTP client** | `reqwest` (via `teloxide::net::default_reqwest_settings()`) |
| **Web server** | `axum` (bundled for webhook support, currently unused in favour of polling) |

teloxide is the dominant Rust Telegram crate. It provides:
- `Bot` — the primary API client
- `Dispatcher` / `dptree` — update routing tree
- `BotCommands` derive macro — slash command parsing
- Typed request builders (`.send_message()`, `.edit_message_text()`, etc.)

---

## 2. Bot Token and Config Loading

**Env var name**: `TELEGRAM_BOT_TOKEN`

**Loading order** (`main.rs`):
```rust
// 1. App-specific config dir (XDG-style path from config::env_file())
let env_path = config::env_file();
if env_path.exists() {
    let _ = dotenvy::from_path(&env_path);
}
// 2. .env.local fallback
let _ = dotenvy::from_filename(".env.local")
    .or_else(|_| dotenvy::dotenv());
```

**Bot construction** (`bot.rs`):
```rust
let token = std::env::var("TELEGRAM_BOT_TOKEN")
    .map_err(|_| TelegramError::NoToken)?;

// Configure reqwest client with explicit timeouts
let client = teloxide::net::default_reqwest_settings()
    .timeout(Duration::from_secs(120))          // long-poll read timeout
    .connect_timeout(Duration::from_secs(30))
    .pool_idle_timeout(Duration::from_secs(90))
    .pool_max_idle_per_host(2)
    .build()?;

let bot = Bot::with_client(token, client);
```

Optional webhook port: `TELEGRAM_WEBHOOK_PORT` (defaults to 8443).

---

## 3. Polling vs Webhook

**Active choice: long polling** (`start_polling()`).

Webhook support is scaffolded (ngrok + axum) but the `start()` method immediately
falls back to `start_polling()` with a `warn!`. This is the recommended dev/personal
bot pattern — no public HTTPS endpoint needed.

```rust
Dispatcher::builder(bot, handler)
    .default_handler(|upd| async move { warn!("Unhandled update: {:?}", upd); })
    .error_handler(teloxide::error_handlers::LoggingErrorHandler::with_custom_text("..."))
    .enable_ctrlc_handler()
    .build()
    .dispatch()
    .await;
```

`enable_ctrlc_handler()` registers a graceful shutdown on Ctrl-C.

---

## 4. Update Routing (dptree)

teloxide uses `dptree` for composable handler trees. The routing in `start_polling`:

```rust
let handler = dptree::entry()
    // 1. Inline keyboard button callbacks
    .branch(
        Update::filter_callback_query()
            .endpoint(|bot, q: CallbackQuery| handle_callback(bot, q, state)),
    )
    // 2. Slash commands (/start, /help, /connect, etc.)
    .branch(
        Update::filter_message()
            .filter_command::<Command>()
            .endpoint(|bot, msg, cmd: Command| handle_command(bot, msg, cmd, state)),
    )
    // 3. Unrecognized slash commands (start with '/' but didn't parse)
    .branch(
        Update::filter_message()
            .filter(|msg: Message| msg.text().map(|t| t.starts_with('/')).unwrap_or(false))
            .endpoint(|bot, msg| { bot.send_message(msg.chat.id, "Unknown command...") }),
    )
    // 4. Regular text messages (the main AI dispatch path)
    .branch(
        Update::filter_message()
            .filter(|msg: Message| msg.text().map(|t| !t.starts_with('/')).unwrap_or(false))
            .endpoint(|bot, msg| handle_message(bot, msg, state)),
    );
```

**`Command` enum** is derived with `#[derive(BotCommands)]`:
```rust
#[derive(BotCommands, Clone, Debug)]
#[command(rename_rule = "lowercase", description = "Available commands:")]
pub enum Command {
    #[command(description = "Start the bot")]
    Start(String),   // String captures the payload after /start
    #[command(description = "Connect to project")]
    Connect(String), // String captures arguments after /connect
    // ...
}
```

---

## 5. Message Received → AI Agent Dispatch

`handle_message()` in `handlers.rs` is the main path:

```
Telegram user sends text
    |
    v
handle_message(bot, msg, state)
    |
    +-- Authorization check (state.is_authorized(chat_id))
    |
    +-- Forum topic routing (group mode — separate path)
    |
    +-- @-alias routing ("@project-name task text" → named session)
    |
    +-- Reply-chain routing (reply to bot's response → same session)
    |
    +-- Require active session (state.has_session(chat_id))
    |
    +-- Event-driven adapter path (mpm-sdk):
    |       state.try_send_event_driven(bot, chat_id, text, msg_id)
    |           → immediate async response via adapter callbacks
    |
    +-- tmux adapter path (default):
            state.send_message(chat_id, text, Some(msg.id))
                → writes text to the connected tmux session
                → sets session.is_waiting = true
                → response collected by background poll loop
```

**Authorization**: chat IDs are stored in a persistent file (`authorized_chats_file()`).
Pairing is done with a one-time code via `/pair <CODE>`.

---

## 6. Response Sent Back to Telegram

ai-commander uses a **background polling loop** rather than request/response,
because the AI backend (tmux + Claude Code / mpm) is async and output is streamed
over time.

### The Poll Loop (`poll_output_loop` in `bot.rs`)

Runs every 500ms. For each session with `is_waiting = true`:

1. Sends `ChatAction::Typing` indicator (throttled to once per 5s per chat).
2. Calls `state.poll_output(chat_id)` → returns `PollResult`.
3. Acts on the result:

```rust
PollResult::Progress(msg)         → edit or send a silent progress message
PollResult::IncrementalSummary(s) → edit or send a summary (every 50 lines)
PollResult::ProgressiveSummary(s) → edit progress with LLM summary text
PollResult::Summarizing           → show "Summarizing..." progress
PollResult::Complete(text, msg_id, thread_id) → send final response
PollResult::SelectorDetected(sel) → send inline keyboard prompt
PollResult::NoOutput              → nothing yet, keep polling
```

### Sending the Final Response

```rust
// ParseMode::Html is used throughout (NOT MarkdownV2)
send_long_message(
    &bot,
    chat_id,
    &response,
    teloxide::types::ParseMode::Html,
    target_thread_id,
    reply_params,       // ReplyParameters::new(original_msg_id)
    keyboard,           // Optional InlineKeyboardMarkup
    false,              // not silent (final response)
    effect_id,          // confetti effect in private chats
    features.max_message_length, // 4096
).await
```

`send_long_message` splits on newline boundaries at 4096 chars using `split_message()`.
Reply parameters and inline keyboard are only attached to the **last** chunk.

---

## 7. Message Formatting

**Parse mode**: `ParseMode::Html` exclusively (not `Markdown` or `MarkdownV2`).

HTML tags used:
- `<b>bold</b>` for emphasis
- `<code>inline code</code>`
- `<pre><code>code blocks</code></pre>`
- `<a href="...">link text</a>` for deep links

**Escaping**: `teloxide::utils::html::escape(&text)` for any user-supplied strings.

**Long `<pre>` blocks**: wrapped in `<blockquote expandable>` when > 300 chars,
so Telegram clients collapse them (old clients degrade to plain blockquote).

**Link previews**: disabled on all progress/notification messages:
```rust
.link_preview_options(LinkPreviewOptions { is_disabled: true, .. })
```

**Message effects**: confetti (`5046509195757842739`) in private chats only on completion.

**Reactions**: `bot.set_message_reaction(chat_id, msg_id, ["👍"])` on the original
user message when the agent completes.

---

## 8. Inline Keyboards (Callback Queries)

Used for interactive option detection (the AI response contains numbered options):

```rust
// Detect "a) Option" / "1. Option" / "Yes/No" patterns in response text
let detected_options = OptionDetector::detect_options(&response);
let keyboard = detected_options.as_ref().map(|o| {
    InlineKeyboardMarkup::new(
        options.iter().map(|opt| {
            vec![InlineKeyboardButton::callback(
                format!("{}) {}", opt.key, opt.label),
                format!("option:{}", opt.key),   // callback_data
            )]
        }).collect()
    )
});
```

Callback handler receives `CallbackQuery`, parses `callback_data`, and sends
the selected option text back to the session.

---

## 9. Session State Management

`UserSession` struct (`session.rs`) tracks per-chat state:

```rust
pub struct UserSession {
    pub chat_id: ChatId,
    pub project_path: String,
    pub tmux_session: String,          // tmux session name
    pub response_buffer: Vec<String>,  // accumulates AI output lines
    pub is_waiting: bool,              // awaiting AI response
    pub pending_message_id: Option<MessageId>, // for reply threading
    pub thread_id: Option<ThreadId>,   // forum topic (group mode)
    pub event_handle: EventHandleState, // mpm-sdk event-driven handle
    // ... progress tracking, incremental summary state, etc.
}
```

**Persistence**: sessions are serialized to JSON via `PersistedSession` and restored
on bot restart (max 24h TTL). File: runtime state dir.

**State container**: `Arc<RwLock<HashMap<ChatId, UserSession>>>` in `TelegramState`.

**Two adapter paths** coexist:
- **tmux** (default): writes to tmux, polls for new output
- **event-driven** (mpm-sdk): `SessionHandle` with async callbacks, bypasses polling

---

## 10. Typing Throttle

A `TypingThrottle` component prevents Telegram rate-limit errors:
- Max one `sendChatAction` per chat per 5 seconds
- Backs off on `Retry-After` responses from Telegram

```rust
state.typing_throttle.send_if_allowed(&bot, chat_id, thread_id).await;
```

---

## 11. Notifications (Cross-Channel)

A second background task (`poll_notifications_loop`) runs every 2s and broadcasts
file-based notifications to all authorized chats:

```rust
get_unread_notifications("telegram")  // reads from a shared notification dir
// sends to all authorized chat IDs
// appends deep links: https://t.me/{bot_username}?start=connect_{session}
mark_notifications_read("telegram", &ids)
```

---

## Recommendations for open-mpm Port

### Crate selection
Use **teloxide 0.17** (same version). Add to `Cargo.toml`:
```toml
teloxide = { version = "0.17", features = ["macros"] }
tokio = { workspace = true }
```
Webhook feature (`webhooks-axum`) is optional — start with polling only.

### Key patterns to adopt

1. **`Bot::with_client(token, client)`** with explicit reqwest timeout config —
   avoids hanging on slow connections.

2. **`dptree::entry().branch(...)`** routing tree — clean separation of slash
   commands, callbacks, and free-text messages.

3. **`#[derive(BotCommands)]`** for slash command parsing — zero-boilerplate.

4. **`ParseMode::Html`** and `teloxide::utils::html::escape()` — simpler than
   MarkdownV2 escaping rules.

5. **Background poll loop with `PollResult` enum** — natural fit for async AI
   backends that stream output over time. Alternatively, for an event-driven
   backend (like open-mpm's IPC), a `tokio::sync::mpsc` channel from the agent
   to the bot task is cleaner.

6. **`ReplyParameters::new(original_msg_id)`** — threads bot replies to the user's
   message in Telegram.

7. **`send_long_message` with 4096-char split** — Telegram hard limit for bot
   messages.

8. **`enable_ctrlc_handler()`** on `Dispatcher::builder` — graceful shutdown.

### For open-mpm specifically

Since open-mpm dispatches tasks to sub-agents via NDJSON IPC and collects a single
final result (rather than streaming tmux output), a simpler response pattern works:

```
User message → handle_message → spawn task:
    1. Send task to PM orchestrator (existing IPC)
    2. Await result on mpsc channel
    3. bot.send_message(chat_id, result).reply_parameters(...).await
```

No polling loop needed — the IPC result arrives via the existing `tokio::process`
pipeline. Use a `tokio::spawn` per message and send back on completion.

---

*Research captured to `docs/research/ai-commander-telegram-client.md`*
