# OrcaCLI

A consolidated CLI agent frontend for OrcaRouter with agentic tool capabilities, written in Rust.

## Features

- **Fast & lightweight** — Compiled Rust binary, no Node.js required
- **Same functionality** — All features from the Node.js version
- **Multiple interaction modes** — Q&A, interactive chat, agentic tasks
- **File operations** — LLM can read, write, and modify local files
- **Model selection** — Choose from 200+ models or use adaptive routing
- **Agentic loops** — Multi-turn execution with tool calling

## Installation

### Prerequisites
- Rust 1.70+ (install from [rustup.rs](https://rustup.rs))
- An OrcaRouter API key from [orcarouter.ai](https://www.orcarouter.ai)

### Build from Source

```bash
cargo build --release
```

The binary will be at `target/release/orca` (or `orca.exe` on Windows).

### Install Globally

```bash
cargo install --path .
```

Then use `orca` from anywhere:

```bash
orca login
orca ask "Hello"
```

## Usage

### Commands

#### `login`
Set up or update your OrcaRouter API key.

```bash
orca login
```

#### `logout`
Remove your stored API key.

```bash
orca logout
```

#### `status`
Check your configuration.

```bash
orca status
```

#### `models`
List available models from OrcaRouter (shows first 20).

```bash
orca models
```

#### `model [name]`
View or set the persistent default model, so you don't need to pass `-m` on every call.

```bash
# Show the current default
orca model

# Set the default model
orca model anthropic/claude-opus-4.5

# Clear the default (falls back to orcarouter/auto)
orca model --clear
```

Once set, `ask`, `chat`, and `agent` all use this default unless overridden with `-m`/`--model` for that specific call.

#### `max-iterations [value]`
View or set the persistent default for how many tool-calling iterations `agent` may run before giving up.

```bash
# Show the current default
orca max-iterations

# Set the default
orca max-iterations 20
```

Defaults to 20. Overridden per call with `--max-iterations` on `agent`.

#### `effort-level [value]`
View or set the persistent default reasoning effort (`low`, `medium`, or `high`) sent to models that support it. Applies to `ask`, `chat`, and `agent`.

```bash
# Show the current effort level
orca effort-level

# Set the default
orca effort-level high

# Clear it (falls back to the provider default)
orca effort-level --clear
```

When an effort level is set, `ask`, `chat`, and `agent` label responses as `<model> (<effort>)` instead of just `<model>`, so you can see which effort level produced a given answer.

#### `approval`
Configure approval settings for agentic actions. By default, the agent prompts for approval before reading files, writing files, or running terminal commands.

```bash
# Show current approval settings
orca approval

# Configure individual approvals (use on/off, true/false, yes/no, or 1/0)
orca approval read off      # Auto-approve file reads
orca approval write on      # Prompt before file writes
orca approval terminal on   # Prompt before terminal commands

# Set all approvals at once
orca approval all off       # Auto-approve everything (use with caution)
orca approval all on        # Prompt for all actions
```

#### `ask <prompt>`
Send a single prompt to the LLM.

```bash
orca ask "What is OrcaRouter?"

# Specify a model
orca ask "Explain quantum computing" -m anthropic/claude-opus-4.5

# Adjust temperature
orca ask "Write a poem" -t 1.5
```

#### `chat`
Start an interactive multi-turn conversation.

```bash
orca chat
# Type exit to quit
```

#### `agent <task>`
Run an agentic task where the LLM can use tools (read/write files).

```bash
# Create a new file
orca agent "Create a file called hello.rs that prints 'Hello, world!'"

# Modify existing code
orca agent "Read src/main.rs, identify improvements, and write an optimized version"

# Show detailed iteration logs
orca agent "Create utils.rs with a reverse array function" -v

# Override the default max iterations for this call
orca agent "Generate project structure" --max-iterations 30
```

## Agentic Tools

When running `agent` commands, the LLM has access to these tools:

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

Configuration is stored at `~/.orcacli/config.json`:

```json
{
  "api_key": "sk-orca-...",
  "base_url": "https://api.orcarouter.ai/v1",
  "default_model": "anthropic/claude-opus-4.5",
  "approval": {
    "read_disk": true,
    "write_disk": true,
    "terminal": true
  },
  "max_iterations": 20,
  "effort_level": "high"
}
```

- `default_model` is managed via `orca model` and is used by `ask`, `chat`, and `agent` when `-m`/`--model` isn't passed.
- `approval` settings control whether the agent prompts before performing actions. Managed via `orca approval`.
- `max_iterations` is managed via `orca max-iterations` and is the default for `agent` when `--max-iterations` isn't passed.
- `effort_level` is managed via `orca effort-level` and is sent as `reasoning_effort` for `ask`, `chat`, and `agent` when set.

## Examples

### Generate and save code

```bash
orca agent "Write a function that calculates fibonacci numbers and save it to math.rs"
```

### Multi-file project setup

```bash
orca agent "Create a basic Rust project structure with Cargo.toml, src/main.rs, and src/lib.rs"
```

### Fix existing code

```bash
orca agent "Read main.rs, find any issues, and write a corrected version"
```

### Using different models

```bash
# Claude for code review
orca agent "Read app.rs and provide detailed code review feedback" -m anthropic/claude-opus-4.5

# GPT-4 for complex logic
orca agent "Create an algorithm to solve the traveling salesman problem" -m openai/gpt-4o

# Adaptive routing (default)
orca agent "Generate boilerplate code" -m orcarouter/auto
```

## Rust vs Node.js Comparison

| Feature | Rust | Node.js |
|---------|------|---------|
| Binary Size | ~20 MB | N/A (needs Node.js) |
| Startup Time | <100ms | ~300ms |
| Memory | ~20 MB | ~100 MB |
| Dependencies | Compiled in | Runtime (node_modules) |
| Installation | Single binary | npm install required |
| Cross-platform | Yes | Yes |

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

Run `orca login` and enter your key from [orcarouter.ai](https://www.orcarouter.ai/console/keys).

### "Model not found"

Run `orca models` to see available models, then use the correct model ID with `-m`.

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
- API keys are stored in `~/.orcacli/config.json` — add it to `.gitignore`

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

Rust version is significantly faster:

```bash
# Node.js version
time node index.js ask "Hello"
# real    0m0.280s

# Rust version
time orca ask "Hello"
# real    0m0.015s
```

## License

MIT

## Support

For issues with OrcaRouter itself, visit [docs.orcarouter.ai](https://docs.orcarouter.ai)
