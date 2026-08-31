# AI Comms CLI

An OpenAI-compatible CLI frontend for any LLM provider, with agentic tool capabilities, written in Rust. Defaults to OpenRouter, but works with any OpenAI-compatible service (OrcaRouter, Together, Groq, self-hosted gateways, etc) via `comms endpoint` — see [Using other providers](#using-other-providers).

## Features

- **Fast & lightweight** — Compiled Rust binary, single executable with no runtime dependencies
- **Multiple interaction modes** — Q&A, interactive chat, agentic tasks, and a full-screen TUI
- **Streaming responses** — Replies appear as they're generated rather than all at once
- **File operations** — LLM can read, write, and modify local files
- **Model selection** — Choose from 200+ models or use adaptive routing
- **Agentic loops** — Multi-turn execution with tool calling
- **Persistent sessions** — `session`/`tui` conversations are saved to SQLite and resumable across restarts
- **Secure credential storage** — API keys live in your OS keychain, not a plaintext file

## Manual Installation

### Prerequisites
- Rust 1.70+ (install from [rustup.rs](https://rustup.rs))
- An API key for your provider — defaults to OpenRouter, get one from [openrouter.ai/keys](https://openrouter.ai/keys) (or see [Using other providers](#using-other-providers))

### Build from Source

```bash
cargo build --release
```

The binary will be at `target/release/comms` (or `comms.exe` on Windows).

### Install Globally

```bash
cargo install --path .
```

Then use `comms` from anywhere:

```bash
comms login
comms ask "Hello"
```

## Usage

### Commands

#### `login`
Set up or update your API key. The key is stored in your OS keychain (macOS Keychain, Windows Credential Manager, or the Linux Secret Service) rather than in a plaintext config file.

```bash
comms login
```

#### `logout`
Remove your stored API key from the OS keychain.

```bash
comms logout
```

#### `status`
Check your configuration.

```bash
comms status
```

#### `models`
List available models from your configured provider (shows first 20).

```bash
comms models
```

#### `model [name]`
View or set the persistent default model, so you don't need to pass `-m` on every call.

```bash
# Show the current default
comms model

# Set the default model
comms model anthropic/claude-opus-4.5

# Clear the default (falls back to openrouter/auto)
comms model --clear
```

Once set, `ask`, `session`, and `agent` all use this default unless overridden with `-m`/`--model` for that specific call.

**Per-session models.** A session remembers the model it's using. Passing
`--model` when resuming, or running `/model` inside the TUI, switches it *and*
records it — so resuming later with no flag picks up where you left off rather
than reverting. Each stored reply still records the model that produced it,
even though the transcript itself — in `session`, `tui`, and `sessions show`
alike — no longer prints a model label on every line; `/model` shows what the
session is using *now*.

#### `max-iterations [value]`
View or set the persistent default for how many tool-calling iterations `agent` may run before giving up.

```bash
# Show the current default
comms max-iterations

# Set the default
comms max-iterations 20
```

Defaults to 20. Overridden per call with `--max-iterations` on `agent`, or
persistently per session with `/max-iterations` inside a `session`/`tui`
conversation — see [Per-session models](#model-name) for how that
precedence works.

#### `temperature [value]`
View or set the persistent default sampling temperature (0-2) sent to models that support it.

```bash
# Show the current default
comms temperature

# Set the default
comms temperature 1.2
```

Defaults to 0.7. Overridden per call with `--temperature` on `session`,
`agent`, or `tui`, or persistently per session with `/temperature` (or its
`/temp` shorthand) inside a `session`/`tui` conversation — see
[Per-session models](#model-name) for how that precedence works.

#### `effort-level [value]`
View or set the persistent default reasoning effort (`low`, `medium`, or `high`) sent to models that support it. Applies to `ask`, `session`, and `agent`.

```bash
# Show the current effort level
comms effort-level

# Set the default
comms effort-level high

# Clear it (falls back to the provider default)
comms effort-level --clear
```

When an effort level is set, `ask`, `session`, and `agent` label responses as `<model> (<effort>)` instead of just `<model>`, so you can see which effort level produced a given answer.

#### `endpoint [url]`
View or set the API base URL, so you can point `comms` at any OpenAI-compatible service instead of OpenRouter (OrcaRouter, Together, Groq, a self-hosted gateway, etc).

```bash
# Show the current endpoint
comms endpoint

# Point at OrcaRouter
comms endpoint https://api.orcarouter.ai/v1

# Clear it (falls back to the OpenRouter default)
comms endpoint --clear
```

Switching endpoints doesn't switch your API key or default model automatically — run `comms login` to set the new provider's key, and `comms model` to set a model it actually serves.

#### `effort-style [value]`
View or set how the reasoning effort level (`comms effort-level`) is serialized in requests, since providers disagree on the shape:

- `nested` (default) — sends `reasoning: { effort: "<level>" }`, as OpenRouter expects.
- `flat` — sends `reasoning_effort: "<level>"` at the top level, as OrcaRouter expects.
- `none` — omits effort entirely, for providers that reject unrecognized fields.

```bash
comms effort-style
comms effort-style flat
comms effort-style --clear
```

#### `headers`
View or manage extra HTTP headers sent with every API request, useful for providers with optional attribution headers (e.g. OpenRouter's `HTTP-Referer`/`X-Title`).

```bash
# Show current extra headers
comms headers

# Set a header
comms headers set HTTP-Referer https://myapp.example.com
comms headers set X-Title "My App"

# Remove one
comms headers unset HTTP-Referer
```

#### `approval`
Configure approval settings for agentic actions. By default, the agent prompts for approval before reading files, writing files, or running terminal commands.

```bash
# Show current approval settings
comms approval

# Configure individual approvals (use on/off, true/false, yes/no, or 1/0)
comms approval read off      # Auto-approve file reads
comms approval write on      # Prompt before file writes
comms approval terminal on   # Prompt before terminal commands

# Set all approvals at once
comms approval all off       # Auto-approve everything (use with caution)
comms approval all on        # Prompt for all actions
```

**Per-session approval.** These commands set the default new sessions start
with. A session remembers its own approval settings too: running `/approval`
inside it (see [`session`](#session) or [`tui`](#tui)) switches and records
them for that session alone, the same way `/model` does for models — so
resuming later with no override picks up where you left off rather than
reverting to the configured default.

#### `ask <prompt>`
Send a single prompt to the LLM.

```bash
comms ask "What's the capital of France?"

# Specify a model
comms ask "Explain quantum computing" -m anthropic/claude-opus-4.5
```

#### `session`
Start an interactive, persistent conversation — the line-based counterpart to
`tui`, with the same experience minus the full-screen UI. It's saved
automatically as you go (see [Session Persistence](#session-persistence)), so
you can pick it back up later.

Every new session starts in plain **ask mode**; type `/agent` from inside it
to turn on tools (read/write files, run commands) for the rest of the
session, `/ask` to turn them back off. Also supported, matching the TUI
exactly:

| Command | Does |
|---|---|
| `/model <name>` | Switch the model for the rest of the session, and remember it |
| `/model` | Show the model currently in use |
| `/agent` | Turn on tool-calling for the rest of the session |
| `/ask` | Turn tool-calling back off |
| `/effort <level>` | Switch reasoning effort (`low`/`medium`/`high`) for the rest of the session |
| `/effort clear` | Fall back to the configured default effort |
| `/verbose` | Toggle showing full tool call arguments/results instead of a one-line notice |
| `/max-iterations <n>` | Switch the tool-calling iteration cap per turn (agent mode only), and remember it |
| `/max-iterations clear` | Fall back to the configured default cap |
| `/temperature <n>` (or `/temp <n>`) | Switch the sampling temperature for the rest of the session, and remember it |
| `/temperature clear` (or `/temp clear`) | Fall back to the configured default temperature |
| `/approval <read\|write\|terminal\|all> <on\|off>` | Switch a tool-approval gate for the rest of the session, and remember it |
| `/approval` | Show the approval gates currently in use |

```bash
comms session
# Type exit to quit

# Override the default model for a new session (ignored when resuming —
# a resumed session always keeps its own saved model)
comms session -m anthropic/claude-opus-4.5

# Override the default max tool-calling iterations per turn while in agent mode
comms session --max-iterations 30

# Resume a previous session by id (or a unique prefix of it) — works
# whether that session is currently in ask or agent mode
comms session --resume a1b2c3d4

# Or omit the id to pick from a numbered list of all your saved sessions
comms session --resume
```

#### `agent <task>`
Run a single agentic task where the LLM can use tools (read/write files) — one-shot, not a persistent session. For a continuous conversation with tools, use `session` and `/agent`.

```bash
# Create a new file
comms agent "Create a file called hello.rs that prints 'Hello, world!'"

# Modify existing code
comms agent "Read src/main.rs, identify improvements, and write an optimized version"

# Show detailed iteration logs
comms agent "Create utils.rs with a reverse array function" -v

# Override the default max iterations for this call
comms agent "Generate project structure" --max-iterations 30
```

#### `tui`
A full-screen terminal UI. Unlike the line-based `session`, it owns the
screen, which is what lets the input box stay live while a reply streams in,
tool approvals appear inline, and a running turn be interrupted. Otherwise
the two are functionally identical — same commands, same settings, same
saved sessions, interchangeably resumable from either.

Run bare, it opens on a **launch screen**: start a new session, jump straight
back into a recent one, or go to a sessions browser covering all of them. The
flags skip it and go straight into a conversation.

```bash
comms tui                      # launch screen
comms tui --session            # straight into a new session (ask mode)
comms tui --resume a1b2c3d4    # straight into a saved session
```

`--session` and `--resume` are mutually exclusive. Every new session starts in
plain ask mode; use `/agent` from inside it to turn tools on (see
**Commands** below) — there's no separate "agent" launch flag anymore. A
resumed session picks back up in whichever mode, model, and effort level it
was last left in, regardless of any of the flags above.

Choosing "New session" from the launch screen (as opposed to `--session`,
which skips straight in) first asks for a title; leave it blank to fall back
to the usual behavior of naming the session from your first message.

**Launch screen / sessions browser**

| Key | Does |
|---|---|
| `↑` / `↓` (or `k` / `j`) | Move the selection |
| `Enter` | Open the selected row |
| `r` | Rename a session (sessions browser only) |
| `d` | Delete a session (sessions browser only, asks to confirm) |
| `Esc` | Back to the launch screen |
| `q` | Quit |

**In a conversation**

| Key | Does |
|---|---|
| `Enter` | Send. If a reply is still streaming, the message is queued and sent when it finishes |
| `Esc` | Cancel the in-flight turn (kills a running tool command too) |
| `↑` / `↓` | Recall previous messages into the input box |
| type an answer, `Enter` | Answer a tool approval prompt — `y`/`yes` allows, anything else (including blank) denies |
| `PgUp` / `PgDn` / `End` | Scroll the transcript; `End` re-pins to the newest |
| Mouse wheel | Also scrolls the transcript — `↑`/`↓` stay dedicated to prompt history |
| `Ctrl-B` | Back to the launch screen (the session is saved) |
| `Ctrl-C` | Quit |

**Commands.** Type these in the message box instead of a message:

| Command | Does |
|---|---|
| `/model <name>` | Switch the model for the rest of the session, and remember it |
| `/model` | Show the model currently in use |
| `/agent` | Turn on tool-calling (read/write files, run commands) for the rest of the session |
| `/ask` | Turn tool-calling back off |
| `/effort <level>` | Switch reasoning effort (`low`/`medium`/`high`) for the rest of the session |
| `/effort clear` | Fall back to the configured default effort |
| `/verbose` | Toggle showing full tool call arguments/results instead of a one-line notice |
| `/max-iterations <n>` | Switch the tool-calling iteration cap per turn (agent mode only), and remember it |
| `/max-iterations clear` | Fall back to the configured default cap |
| `/temperature <n>` (or `/temp <n>`) | Switch the sampling temperature for the rest of the session, and remember it |
| `/temperature clear` (or `/temp clear`) | Fall back to the configured default temperature |
| `/approval <read\|write\|terminal\|all> <on\|off>` | Switch a tool-approval gate for the rest of the session, and remember it |
| `/approval` | Show the approval gates currently in use |

Only recognized commands are intercepted — a message that merely starts with a
slash (`/etc/hosts`, say) is sent as normal text. All of the above persist to
the session, so they stick across `Ctrl-B`/`--resume` too.

Sessions are saved exactly as the other commands save them, so a `tui`
session can be resumed with `comms session --resume` and vice versa — mode,
model, and effort level all carry over either way. Opening a conversation
and leaving without saying anything discards it rather than leaving an empty
"Untitled" in your session list.

#### `stream [on|off]`
Whether replies stream in as they're generated. On by default. Turn it off for
providers that handle streaming — particularly streaming alongside tool calls —
badly; the CLI then waits for the whole reply as it used to.

```bash
comms stream          # show the current setting
comms stream off      # wait for complete replies
comms stream on
```

#### `sessions`
List, inspect, or delete saved `session`/`tui` sessions.

```bash
# List all saved sessions (id prefix, kind, model, title)
comms sessions list

# Show a session's full message history
comms sessions show a1b2c3d4

# Delete a saved session
comms sessions delete a1b2c3d4
```

## Agentic Tools

When running `agent`, or `session`/`tui` in agent mode, the LLM has access to these tools:

### `write_file`
Write or append content to a file.

### `read_file`
Read the contents of a file.

### `list_files`
List files in a directory.

### `replace_in_file`
Replace text in an existing file.

### `run_terminal_command`
Execute a shell command and return the output. Supports custom working directory and timeout.

## Configuration

Configuration is stored at `~/.comms/config.json`:

```json
{
  "base_url": "https://openrouter.ai/api/v1",
  "default_model": "anthropic/claude-opus-4.5",
  "approval": {
    "read_disk": true,
    "write_disk": true,
    "terminal": true
  },
  "max_iterations": 20,
  "temperature": 0.7,
  "effort_level": "high",
  "effort_style": "nested",
  "extra_headers": {}
}
```

- Your API key is **not** in this file — `comms login`/`logout` store and remove it from the OS keychain instead (see [Security](#security)). If you have an old config with a plaintext `api_key` field, the next command that loads config transparently migrates it into the OS keychain and rewrites the file without it.
- `base_url` is managed via `comms endpoint` and is the API endpoint used by every command. Defaults to OpenRouter; point it at any OpenAI-compatible service.
- `default_model` is managed via `comms model` and is used by `ask`, `session`, and `agent` when `-m`/`--model` isn't passed.
- `approval` settings control whether the agent prompts before performing actions. Managed via `comms approval`.
- `max_iterations` is managed via `comms max-iterations` and is the default for `agent` when `--max-iterations` isn't passed.
- `temperature` is managed via `comms temperature` and is the default for `ask`, `session`, `agent`, and `tui` when `--temperature` isn't passed.
- `effort_level` is managed via `comms effort-level` and is sent for `ask`, `session`, and `agent` when set, shaped according to `effort_style`.
- `effort_style` is managed via `comms effort-style` and controls whether the effort level is sent flat, nested, or omitted (see [`effort-style`](#effort-style-value)).
- `extra_headers` is managed via `comms headers` and is merged into every API request.

### Using other providers

AI Comms CLI talks to any service exposing an OpenAI-compatible `/chat/completions` and `/models` API over `Authorization: Bearer` auth — this covers OpenRouter, OrcaRouter, Together, Groq, Fireworks, and self-hosted gateways (vLLM, Ollama's OpenAI shim, LM Studio). It does not cover providers with a different auth scheme or URL shape, like Azure OpenAI.

To switch to OrcaRouter, for example:

```bash
comms endpoint https://api.orcarouter.ai/v1
comms login                          # enter your OrcaRouter key
comms model orcarouter/auto          # or any model OrcaRouter serves
comms effort-style flat              # OrcaRouter expects reasoning as a top-level field
```

Only one provider is active at a time today — switching back to OpenRouter means re-running `comms endpoint`, `comms login`, `comms model`, and `comms effort-style` for it. Named provider profiles (switch between saved providers with one command) are tracked in `TODO.md`.

## Session Persistence

`session` and `tui` conversations are saved automatically to a SQLite database at `~/.comms/chats.db`. Every message (yours, the assistant's, and any tool calls/results while in agent mode) is written as the conversation happens, so you don't lose anything if you exit or your terminal closes — including a turn you cancelled partway through.

Each session gets an id (a UUID) and a title derived from your first message (or one you choose up front, in the TUI). Use:

- `comms sessions list` to see saved sessions (shown by 8-character id prefix, kind, model, and title)
- `comms sessions show <id>` to view a session's full transcript
- `comms sessions delete <id>` to remove one
- `comms session --resume <id>` to continue a saved session — works for one currently in ask mode or agent mode alike, since mode is just session state now, not a separate command
- `comms session --resume` with no id to pick one from a numbered list of all your saved sessions

Any unique prefix of a session's id works wherever a full id is expected.

## Examples

### Generate and save code

```bash
comms agent "Write a function that calculates fibonacci numbers and save it to math.rs"
```

### Multi-file project setup

```bash
comms agent "Create a basic Rust project structure with Cargo.toml, src/main.rs, and src/lib.rs"
```

### Fix existing code

```bash
comms agent "Read main.rs, find any issues, and write a corrected version"
```

### Using different models

```bash
# Claude for code review
comms agent "Read app.rs and provide detailed code review feedback" -m anthropic/claude-opus-4.5

# GPT-4 for complex logic
comms agent "Create an algorithm to solve the traveling salesman problem" -m openai/gpt-4o

# Adaptive routing (default)
comms agent "Generate boilerplate code" -m openrouter/auto
```

## Building for Different Platforms

```bash
# Build for Windows (from macOS/Linux)
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu

# Build for macOS (from other platforms)
rustup target add x86_64-apple-darwin
cargo build --release --target x86_64-apple-darwin

# Build for Linux (from other platforms)
rustup target add x86_64-unknown-linux-gnu
cargo build --release --target x86_64-unknown-linux-gnu
```

## Troubleshooting

### "API key not configured"

Run `comms login` and enter your key from [openrouter.ai/keys](https://openrouter.ai/keys).

### "Model not found"

Run `comms models` to see available models, then use the correct model ID with `-m`.

### Build errors on macOS

```bash
xcode-select --install
```

### Build errors on Linux

```bash
# Ubuntu/Debian
sudo apt-get install build-essential

# Fedora/RHEL
sudo dnf groupinstall "Development Tools"
```

## Security

- File operations are restricted to your current working directory and home directory
- API keys are stored in your OS keychain (macOS Keychain, Windows Credential Manager, or the Linux Secret Service via `keyring`), not in a plaintext file. An older `~/.comms/config.json` with a plaintext `api_key` field is migrated into the keychain automatically the next time you run any `comms` command, and the field is stripped from the file afterward
- `session`/`tui` history (including tool calls and their results) is stored unencrypted in `~/.comms/chats.db` — add it to `.gitignore` and avoid pasting secrets into a session if you plan to keep or share the database file

## Development

To modify the code:

```bash
# Run in debug mode
cargo run -- ask "Hello"

# Run tests
cargo test

# Format code
cargo fmt

# Lint
cargo clippy
```

## Performance

```bash
time comms ask "Hello"
# real    0m0.015s
```

## License

MIT

## Support

For issues with a specific provider's API itself (rate limits, billing, model availability), see that provider's own docs — e.g. [openrouter.ai/docs](https://openrouter.ai/docs) for the default OpenRouter endpoint.
