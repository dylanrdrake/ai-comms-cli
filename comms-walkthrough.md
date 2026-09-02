# AI Comms CLI — How It Works (walkthrough for a Rust-curious developer)

## The big picture

Everything hangs off one idea: **a conversation is just a `Vec<ChatMessage>` that you POST to an OpenAI-compatible server, get a reply back, and maybe loop on.** The three "modes" are just different front ends over that same loop:

- `ask` — send once, print, done.
- `agent` — send, and if the reply contains `tool_calls`, execute them, append the results, send again (up to `max_iterations`).
- `session`/`tui` — the same, but the message list lives in a `ChatSession` that persists every turn to SQLite so you can resume later.

## The modules, what they actually do

**`main.rs`** — entry point. `Cli` is a `clap` derive struct: each subcommand (`Ask`, `Agent`, `Session`, `Login`, …) is a variant of the `Commands` enum with its flags. `main` is `#[tokio::main] async fn main()`; it parses, then `match cli.command` dispatches to a `cmd_*` function. Note the pattern every command follows at the top: `load_config()?` → resolve overrides → `Client::new(config)?`.

**`config.rs`** — `Config` is a plain `serde` struct with `#[serde(default = ...)]` on every field, so a config file with one key (`{"temperature": 1.5}`) is valid and the rest come from seeds. The API key is *not* in the file — `get_api_key()`/`set_api_key()` go through the `keyring` crate to the OS keychain. Also defines `SessionGates`, an `Arc<Mutex<ApprovalSettings>>` + `Arc<AtomicBool>` pair that lets a mid-turn `/approval` or `/sandbox` change reach the running agent loop.

**`client.rs`** — the HTTP layer. `ChatMessage` is the one struct that flows everywhere: `role`, `content`, `tool_calls`, `tool_call_id`, plus `reasoning`/`reasoning_details` for thinking models. `Client::chat()` does a buffered POST to `{base_url}/chat/completions`; `chat_stream()` is the streaming version. The streaming path is the densest part: an `SseDecoder` splits the byte stream into `data:` lines (working on bytes so a multi-byte UTF-8 char split across network chunks survives), and a `StreamAccumulator` reassembles content deltas and — the fiddly bit — tool-call arguments that arrive fragmented across chunks, keyed by `index` so calls come out in order.

**`agent.rs`** — the heart. `run_agent_turn` is the loop:

```rust
while iteration < max_iterations {
    let message = request_turn(...).await?;   // send history + tool defs
    messages.push(message);
    if !message.has_tool_calls() { return Ok(final_response); }
    for tool_call in tool_calls {
        // maybe ask user for approval
        // execute_tool(name, arguments, sandbox).await
        messages.push(ChatMessage { role: "tool", content: result, tool_call_id: ... });
    }
}
```

`request_turn` picks streaming vs buffered based on `client.streaming_enabled()` and normalizes the system prompt (prepended fresh each turn, because Anthropic requires `system` to sit at position 0 and `/agent` can be flipped on mid-conversation).

**`tools.rs`** — the four tools the model can call, defined as JSON schemas (`get_tool_definitions`) and executed in `execute_tool`: `write_file`, `read_file`, `list_files`, `replace_in_file`, `run_terminal_command`. Results are returned as `serde_json::Value` objects and fed back to the model. The sandbox logic lives here: `resolve_for_sandbox` canonicalizes the path (so `..` and symlinks resolve) and `sandbox_refusal` rejects writes outside cwd + home.

**`session.rs`** — `ChatSession` owns the message history *and* the SQLite `Connection`, plus per-session settings (model, kind, effort, temperature, approval, sandbox). `persist_pending()` writes only messages added since `saved_len` — that watermark is how resume doesn't duplicate history. Settings are snapshots: written to the DB at creation, then `/model` etc. mutate that row.

**`store.rs`** — raw SQL: `sessions` and `messages` tables, `create_session`, `append_message`, `load_messages`, `list_sessions`, prefix lookup for `--resume`. Message content, tool calls, and reasoning are encrypted via `crypto::encrypt_opt` before insert; metadata (model, roles, timestamps) is in the clear. `ensure_column` migrates old DBs by `ALTER TABLE ADD COLUMN`.

**`conversation.rs`** — the TUI's worker. A `Conversation` wraps a `ChatSession` in a `tokio::spawn`ed task driven by a `Command` channel and reporting through an `Event` channel. This is what lets the TUI keep rendering while a turn runs, queue messages typed mid-turn, and cancel (abort the turn task — which, via `kill_on_drop`, also reaps a running tool subprocess).

**`ui.rs` / `terminal_ui.rs` / `tui/`** — front ends. `AgentUi` is a trait with `event()` and `approve()`; the CLI's `TerminalAgentUi` prints, the TUI's `ChannelUi` forwards to the worker's event channel. This is the key decoupling: `agent.rs` never prints anything, it just calls `ui.event(...)`, so the same loop drives CLI, TUI, and tests.

## Trace `comms ask "hi"` end to end

1. `Cli::parse()` → `Commands::Ask { prompt: "hi", .. }` → `cmd_ask`.
2. `load_config()` → `Config` from `~/.comms/config.json` (or defaults).
3. `resolve_model`, `resolve_temperature`, `resolve_effort_level` merge CLI flags over config.
4. `Client::new(config)` — pulls the API key from the keychain.
5. Build `vec![ChatMessage { role: "user", content: "hi", .. }]`.
6. `client.chat(...)` → `build_request` → POST `https://openrouter.ai/api/v1/chat/completions` with `Authorization: Bearer <key>`, `stream: true`.
7. The stream yields `StreamEvent::Content` deltas → printed as they arrive, then `Done`.
8. `cmd_ask` prints the wrapped reply.

## Trace `comms agent "write fib to fib.rs"` — the delta

Same start, but `cmd_agent` calls `agent::run_agent` → `run_agent_turn`, and now `tools` are included in the request. The reply likely comes back with `tool_calls` instead of text. Then:

- each call goes through `requires_approval` → maybe `ui.approve(...)` → `execute_tool("write_file", r#"{"filepath":"fib.rs",...}"#, true)`
- the result JSON is appended as `role: "tool"` with the matching `tool_call_id`
- loop repeats until the model replies with text and no tool calls, or the cap hits → `"Agent exceeded max iterations"`.

That `tool_call_id` threading is the whole trick: providers reject a `tool` message that doesn't reference the call that produced it.

## Rust patterns they'll keep seeing

- `#[derive(Parser)]` / `#[derive(Subcommand)]` — clap generates the CLI from structs.
- `#[derive(Serialize, Deserialize)]` + `#[serde(default, skip_serializing_if = "Option::is_none")]` — config and wire formats are plain structs.
- `Result<T, anyhow::Error>` + `?` everywhere; `anyhow!` for ad-hoc errors.
- `async`/`await` + `tokio` for all I/O; `tokio::time::timeout` for timeouts; `tokio::process::Command` with `kill_on_drop(true)` for the terminal tool.
- `impl Stream<Item = Result<StreamEvent>>` + `async_stream::try_stream!` for the SSE stream.
- Tests are extensive and read like documentation — `client.rs`'s chunk-reassembly tests are the easiest way to understand the streaming format.

## Suggested reading order

1. `main.rs` (skim the `cmd_ask` function — smallest complete path)
2. `config.rs` (the `Config` struct + `load_config`)
3. `client.rs` (the `ChatMessage` struct, then `chat`, then the streaming section)
4. `agent.rs` (`run_agent_turn` — the loop)
5. `tools.rs` (what a tool actually is)
6. `session.rs` + `store.rs` (persistence)
7. `conversation.rs` + the `tui/` dir last — it's the most complex front end.

## The one-sentence takeaway

**Once you understand that every feature is just "build a `Vec<ChatMessage>`, send it, append the reply (or execute its tool calls and append those), repeat," you understand the whole tool — everything else is configuration, persistence, and pretty front ends around that loop.**