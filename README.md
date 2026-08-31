# AI Comms CLI

An OpenAI-compatible CLI frontend for any LLM provider, with agentic tool capabilities, written in Rust. Defaults to OrcaRouter, but works with any OpenAI-compatible service (OpenRouter, Together, Groq, self-hosted gateways, etc) via `comms endpoint` — see [Using other providers](#using-other-providers).

## Features

- **Fast & lightweight** — Compiled Rust binary, single executable with no runtime dependencies
- **Multiple interaction modes** — Q&A, interactive chat, agentic tasks
- **File operations** — LLM can read, write, and modify local files
- **Model selection** — Choose from 200+ models or use adaptive routing
- **Agentic loops** — Multi-turn execution with tool calling
- **Persistent sessions** — `chat`/`agent-chat` conversations are saved to SQLite and resumable across restarts
- **Secure credential storage** — API keys live in your OS keychain, not a plaintext file

## Installation

### Prerequisites
- Rust 1.70+ (install from [rustup.rs](https://rustup.rs))
- An API key for your provider — defaults to OrcaRouter, get one from [orcarouter.ai](https://www.orcarouter.ai) (or see [Using other providers](#using-other-providers))

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

# Clear the default (falls back to orcarouter/auto)
comms model --clear
```

Once set, `ask`, `chat`, and `agent` all use this default unless overridden with `-m`/`--model` for that specific call.

#### `max-iterations [value]`
View or set the persistent default for how many tool-calling iterations `agent` may run before giving up.

```bash
# Show the current default
comms max-iterations

# Set the default
comms max-iterations 20
```

Defaults to 20. Overridden per call with `--max-iterations` on `agent`.

#### `effort-level [value]`
View or set the persistent default reasoning effort (`low`, `medium`, or `high`) sent to models that support it. Applies to `ask`, `chat`, and `agent`.

```bash
# Show the current effort level
comms effort-level

# Set the default
comms effort-level high

# Clear it (falls back to the provider default)
comms effort-level --clear
```

When an effort level is set, `ask`, `chat`, and `agent` label responses as `<model> (<effort>)` instead of just `<model>`, so you can see which effort level produced a given answer.

#### `endpoint [url]`
View or set the API base URL, so you can point `comms` at any OpenAI-compatible service instead of OrcaRouter (OpenRouter, Together, Groq, a self-hosted gateway, etc).

```bash
# Show the current endpoint
comms endpoint

# Point at OpenRouter
comms endpoint https://openrouter.ai/api/v1

# Clear it (falls back to the OrcaRouter default)
comms endpoint --clear
```

Switching endpoints doesn't switch your API key or default model automatically — run `comms login` to set the new provider's key, and `comms model` to set a model it actually serves.

#### `effort-style [value]`
View or set how the reasoning effort level (`comms effort-level`) is serialized in requests, since providers disagree on the shape:

- `flat` (default) — sends `reasoning_effort: "<level>"` at the top level, as OrcaRouter expects.
- `nested` — sends `reasoning: { effort: "<level>" }`, as OpenRouter expects.
- `none` — omits effort entirely, for providers that reject unrecognized fields.

```bash
comms effort-style
comms effort-style nested
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

#### `ask <prompt>`
Send a single prompt to the LLM.

```bash
comms ask "What's the capital of France?"

# Specify a model
comms ask "Explain quantum computing" -m anthropic/claude-opus-4.5

# Adjust temperature
comms ask "Write a poem" -t 1.5
```

#### `chat`
Start an interactive multi-turn conversation. The session is saved automatically as you go (see [Session Persistence](#session-persistence)), so you can pick it back up later.

The prompt stays live the whole time — you're never blocked waiting for a response, so you can keep typing as soon as you hit enter. What happens to what you type next depends on whether a response is still in flight when you send it:

| You type... | Nothing in flight | A response is in flight |
|---|---|---|
| a plain message | sent right away | queued, sent automatically once the current response finishes |
| `/steer <message>` | sent right away (the `/steer` prefix is stripped either way) | the in-flight response is cancelled immediately and `<message>` is sent in its place |

```bash
comms chat
# Type exit to quit

# At the prompt, typing a follow-up while the model is still responding
# queues it — it's sent once the current response finishes:
[model] You: also check for edge cases

# Prefixing a message with /steer instead cancels the in-flight response
# and sends this one right away:
[model] You: /steer actually, focus on error handling instead

# Resume a previous session by id (or a unique prefix of it)
comms chat --resume a1b2c3d4

# Or omit the id to pick from a numbered list of your saved chat sessions
comms chat --resume
```

#### `agent <task>`
Run an agentic task where the LLM can use tools (read/write files).

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

#### `agent-chat`
Start an interactive, continuous agentic chat session: like `chat`, but each turn runs the full tool-calling agent loop (read/write files, run commands) against a conversation history that persists for the whole session, so later prompts can refer back to earlier ones. Like `chat`, the session is saved automatically (see [Session Persistence](#session-persistence)).

```bash
comms agent-chat
# Type exit to quit

# Show detailed iteration logs for every turn
comms agent-chat -v

# Override the default max iterations per turn
comms agent-chat --max-iterations 30

# Resume a previous agent-chat session by id (or a unique prefix of it)
comms agent-chat --resume a1b2c3d4

# Or omit the id to pick from a numbered list of your saved agent-chat sessions
comms agent-chat --resume
```

#### `sessions`
List, inspect, or delete saved `chat`/`agent-chat` sessions.

```bash
# List all saved sessions (id prefix, kind, model, title)
comms sessions list

# Show a session's full message history
comms sessions show a1b2c3d4

# Delete a saved session
comms sessions delete a1b2c3d4
```

## Agentic Tools

When running `agent` or `agent-chat` commands, the LLM has access to these tools:

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
  "base_url": "https://api.orcarouter.ai/v1",
  "default_model": "anthropic/claude-opus-4.5",
  "approval": {
    "read_disk": true,
    "write_disk": true,
    "terminal": true
  },
  "max_iterations": 20,
  "effort_level": "high",
  "effort_style": "flat",
  "extra_headers": {}
}
```

- Your API key is **not** in this file — `comms login`/`logout` store and remove it from the OS keychain instead (see [Security](#security)). If you have an old config with a plaintext `api_key` field, the next command that loads config transparently migrates it into the OS keychain and rewrites the file without it.
- `base_url` is managed via `comms endpoint` and is the API endpoint used by every command. Defaults to OrcaRouter; point it at any OpenAI-compatible service.
- `default_model` is managed via `comms model` and is used by `ask`, `chat`, and `agent` when `-m`/`--model` isn't passed.
- `approval` settings control whether the agent prompts before performing actions. Managed via `comms approval`.
- `max_iterations` is managed via `comms max-iterations` and is the default for `agent` when `--max-iterations` isn't passed.
- `effort_level` is managed via `comms effort-level` and is sent for `ask`, `chat`, and `agent` when set, shaped according to `effort_style`.
- `effort_style` is managed via `comms effort-style` and controls whether the effort level is sent flat, nested, or omitted (see [`effort-style`](#effort-style-value)).
- `extra_headers` is managed via `comms headers` and is merged into every API request.

### Using other providers

AI Comms CLI talks to any service exposing an OpenAI-compatible `/chat/completions` and `/models` API over `Authorization: Bearer` auth — this covers OrcaRouter, OpenRouter, Together, Groq, Fireworks, and self-hosted gateways (vLLM, Ollama's OpenAI shim, LM Studio). It does not cover providers with a different auth scheme or URL shape, like Azure OpenAI.

To switch to OpenRouter, for example:

```bash
comms endpoint https://openrouter.ai/api/v1
comms login                          # enter your OpenRouter key
comms model openrouter/auto          # or any model OpenRouter serves
comms effort-style nested            # OpenRouter expects reasoning as a nested object
comms headers set HTTP-Referer https://myapp.example.com   # optional, for OpenRouter's attribution
```

Only one provider is active at a time today — switching back to OrcaRouter means re-running `comms endpoint`, `comms login`, `comms model`, and `comms effort-style` for it. Named provider profiles (switch between saved providers with one command) are tracked in `TODO.md`.

## Session Persistence

`chat` and `agent-chat` sessions are saved automatically to a SQLite database at `~/.comms/chats.db`. Every message (yours, the assistant's, and any tool calls/results in `agent-chat`) is written as the conversation happens, so you don't lose anything if you exit or your terminal closes.

Each session gets an id (a UUID) and a title derived from your first message. Use:

- `comms sessions list` to see saved sessions (shown by 8-character id prefix, kind, model, and title)
- `comms sessions show <id>` to view a session's full transcript
- `comms sessions delete <id>` to remove one
- `comms chat --resume <id>` / `comms agent-chat --resume <id>` to continue a saved session
- `comms chat --resume` / `comms agent-chat --resume` with no id to pick one from a numbered list of your saved sessions of that kind

Each message also records the model and effort level that produced it, so `sessions show` (and the transcript printed when resuming) labels every reply with what actually generated it — accurate even if you resumed with a different `--model` or changed the effort level partway through a session. Older messages saved before this was tracked just fall back to the session's stored model.

Any unique prefix of a session's id works wherever a full id is expected. Resuming a `chat` session with `agent-chat --resume` (or vice versa) is rejected, since the two modes carry different system prompts and tool history.

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
comms agent "Generate boilerplate code" -m orcarouter/auto
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

Run `comms login` and enter your key from [orcarouter.ai](https://www.orcarouter.ai/console/keys).

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
- `chat`/`agent-chat` history (session titles, message content, and tool calls/results) is encrypted at rest in `~/.comms/chats.db` with AES-256-GCM. The encryption key is generated on first use and stored in your OS keychain, the same way the API key is — so the data is unreadable without access to that keychain. `role`, `tool_call_id`, model, and timestamps are left unencrypted since they aren't sensitive. Older, pre-encryption databases are migrated transparently: legacy plaintext rows are read as-is and re-encrypted the next time they're written

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

For issues with a specific provider's API itself (rate limits, billing, model availability), see that provider's own docs — e.g. [docs.orcarouter.ai](https://docs.orcarouter.ai) for the default OrcaRouter endpoint.
