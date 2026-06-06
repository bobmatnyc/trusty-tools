# Telegram Bot Unresponsiveness — Diagnosis (2026-05-05)

## Summary

The Telegram bot compiles and unit tests pass, but `/start` and plain messages
receive no reply at runtime. After full inspection of
`src/telegram/mod.rs`, `src/main.rs`, and `Cargo.toml` the root cause is
almost certainly one of two things:

1. **The bot token is for a bot that was never promoted to have inline/group
   permissions AND `getUpdates` is racing with a Telegram webhook** — unlikely
   if this is a brand-new token, but worth ruling out.
2. **`ctrl::run_pm_task_with_history` hangs or returns an error silently on the
   first call** — the LLM credentials may not be set up for the environment
   where `--telegram` is being run, causing every plain-text message handler to
   call an LLM path that stalls or errors without a visible panic.

No structural bugs in the `dptree` wiring or the `/start` handler were found.
The code is architecturally correct. The problems are operational.

---

## 1. Startup Sequence

```
main() [src/main.rs:685-690]
  -> cli.telegram == true
  -> ctrl::detect_self_project() || current_dir()  => project_path: PathBuf
  -> crate::telegram::run_telegram_bot(project_path).await

run_telegram_bot(project_path) [src/telegram/mod.rs:105-175]
  1. Read TELEGRAM_BOT_TOKEN from env (error if missing)
  2. Build reqwest client with:
       timeout         = 120 s   (long-poll read)
       connect_timeout = 30 s
       pool_idle_timeout = 90 s
       pool_max_idle_per_host = 2
  3. Bot::with_client(token, client)
  4. Arc-wrap project_path and sessions: SessionMap
  5. Build dptree handler:
       branch 1: Update::filter_message + filter_command::<Command>
                 -> handle_command(bot, msg, cmd, sessions, project)
       branch 2: Update::filter_message + filter(text not starting with '/')
                 -> handle_message(bot, msg, sessions, project)
  6. Dispatcher::builder(bot, handler)
       .default_handler(log unhandled at DEBUG)
       .error_handler(LoggingErrorHandler "telegram dispatcher error")
       .enable_ctrlc_handler()
       .build()
       .dispatch()    <-- blocks here in long-polling loop
  7. Awaits SIGINT; returns Ok(()) on clean shutdown
```

No background tokio tasks are spawned. Everything runs in the single
`dispatch()` future on the main tokio runtime.

---

## 2. `/start` Handler

`handle_command` [src/telegram/mod.rs:178-286], `Command::Start` arm:

```rust
let me = bot.get_me().await?;          // API call — can fail with ?
let username = me.username();          // &str, panics if bot has no username
                                       //   (bots always have usernames per TG)
let text = format!("<b>Welcome to @{}</b>\n\n...", html::escape(username));
bot.send_message(chat_id, text)
   .parse_mode(ParseMode::Html)
   .await?;                            // API call — ? propagates to ResponseResult
```

Return type is `ResponseResult<()>` which is `Result<(), RequestError>`.
teloxide's Dispatcher catches these errors and passes them to the
`LoggingErrorHandler`, which logs at `warn` level with the text
"telegram dispatcher error".

**The `/start` handler looks correct.** If it is silent, one of:
- `get_me()` is returning an error (bad token / network)
- `send_message` is returning an error (HTML parse error in Telegram — unlikely
  for this simple string)
- The message is arriving but NOT being parsed as a command (wrong bot username
  in the `/start@botname` form, or the filter chain is matching the wrong branch)

---

## 3. Error Handling — Are Errors Swallowed?

### Command handler errors
Both `get_me().await?` and `send_message(...).await?` use `?` inside
`ResponseResult<()>`. Errors propagate up to the Dispatcher's
`error_handler`, which calls `tracing::warn!`. **They are logged, not
silently dropped.**

To see them you must run with at least `RUST_LOG=warn` (or `RUST_LOG=debug`).
If you are running with `RUST_LOG` unset or set to `error`, these warnings
are suppressed.

### Plain-text message handler errors (handle_message)
```rust
let result = ctrl::run_pm_task_with_history(...).await;
let response_text = match result {
    Ok(reply) => { ... markdown_to_html_safe(&reply) }
    Err(e) => {
        warn!(...);    // logged at warn level
        format!("<b>Error</b>\n<pre>{}</pre>", html::escape(&e.to_string()))
    }
};
send_long_html(&bot, chat_id, user_msg_id, &response_text).await;
Ok(())
```

`handle_message` always returns `Ok(())` — it converts LLM errors to a
formatted error message and tries to send that. **If `send_long_html` itself
fails (e.g., HTML send fails AND plain-text fallback also fails), the errors
are logged at `warn` but the function still returns `Ok(())`.**

The function never panics and never propagates errors to the Dispatcher.

### Conclusion on error visibility
- ALL errors require `RUST_LOG=warn` or higher to be visible.
- If you're running with default log level (`info`) you may still see the
  `warn!` calls because `warn > info`, but `error`-only filters would hide them.
- **No errors are silently dropped at the code level.** They are logged.

---

## 4. Most Likely Reasons the Bot Receives Messages but Does Not Reply

### Reason A — LLM credentials not configured (most probable for plain-text messages)

`handle_message` calls `ctrl::run_pm_task_with_history`. This function:
1. Calls the LLM via whichever credential path is active
   (`CLAUDE_CODE_OAUTH_TOKEN`, `ANTHROPIC_API_KEY`, or `OPENROUTER_API_KEY`)
2. If no credential is set, `pick_credentials(None)` returns `None` and the
   call fails immediately
3. The error is caught, logged as `warn`, and the bot tries to send a
   `<b>Error</b>` message back

If the error reply itself fails to send (e.g., HTML parse rejected by
Telegram), the user sees nothing at all.

**Fix: run with `RUST_LOG=warn cargo run -- --telegram` and watch stderr.**

### Reason B — Long-poll timeout mismatched with Telegram's server timeout

`teloxide` defaults to a 10-second `getUpdates` timeout. The reqwest client
has a 120-second read timeout. These are compatible. However, if the bot was
previously set up with a **webhook** via `setWebhook`, Telegram will NOT
deliver updates via `getUpdates`. The bot would get 0 updates forever.

**Fix: call `https://api.telegram.org/bot<TOKEN>/deleteWebhook` once.**

### Reason C — Commands are not being matched because of bot username suffix

Telegram sends commands in private chats as `/start` and in groups as
`/start@botusername`. The `filter_command::<Command>()` filter handles both
forms correctly (this is built into teloxide). This is unlikely to be the
cause in a private chat but worth knowing for group deployments.

### Reason D — The `--telegram` flag is not being passed

The `--telegram` flag is required. Without it, the process enters the REPL/ctrl
loop, not the bot loop. Verify the binary is invoked as:
```
cargo run -- --telegram
```

### Reason E — `send_long_html` silently fails on the error message itself

If `ctrl::run_pm_task_with_history` errors AND the resulting
`<b>Error</b><pre>...</pre>` HTML message also fails to send (e.g., the error
string itself contains characters that corrupt the HTML), the user sees nothing.
The `warn!` in `send_long_html` is the only signal.

---

## 5. `ChatSession` Structure and Pairing Concepts

```rust
struct ChatSession {
    project_path: PathBuf,
    history: Vec<ConversationTurn>,
}
```

- Keyed by `ChatId` in a `HashMap<ChatId, ChatSession>` wrapped in
  `Arc<Mutex<...>>`.
- Created lazily on first message per chat, using the launch project path.
- `/connect <path>` rebinds the `project_path` but intentionally preserves
  `history`.
- `/clear` empties `history` but keeps `project_path`.
- **There are no `pending_pairs` or pairing concepts** in this implementation.
  The earlier CTO-bot design (`docs/research/ai-commander-telegram-client.md`)
  used a pairing/approval flow, but this implementation does not — every chat
  has direct access.
- The `Mutex` is dropped before calling `ctrl::run_pm_task_with_history`
  (snapshot is taken first), so there is no deadlock risk on the session lock.

---

## 6. Teloxide Version Assessment

- **Resolved version:** `teloxide 0.17.0` / `teloxide-core 0.13.0`
- teloxide 0.17 is the current latest on crates.io as of 2026-05
- The `dptree`, `Dispatcher`, `BotCommands`, `UpdateFilterExt` API used here
  matches the 0.12+ API surface — no breaking changes detected
- `Me::username()` returns `&str` in 0.13 (panics via `.expect()` if the bot
  somehow has no username, which Telegram's API guarantees bots always have)
- No API mismatches found

---

## Recommended Debugging Steps

1. **Run with verbose logging:**
   ```bash
   RUST_LOG=debug cargo run -- --telegram 2>&1 | grep -E "telegram|warn|error"
   ```
2. **Verify no webhook is set:**
   ```bash
   curl "https://api.telegram.org/bot<TOKEN>/getWebhookInfo"
   # If url != "", delete it:
   curl "https://api.telegram.org/bot<TOKEN>/deleteWebhook"
   ```
3. **Verify credentials are set in the environment** where the bot runs:
   ```bash
   echo $CLAUDE_CODE_OAUTH_TOKEN
   echo $ANTHROPIC_API_KEY
   echo $OPENROUTER_API_KEY
   ```
4. **Test the `/start` command in isolation** — it calls only `get_me()` and
   `send_message()`, no LLM. If `/start` is also silent, the issue is with the
   Telegram connection (token, webhook, network), not the LLM.
5. **If `/start` works but plain text does not**, the issue is in the LLM
   credential path or `ctrl::run_pm_task_with_history` hanging.
