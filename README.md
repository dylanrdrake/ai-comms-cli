# Clanker Command Center (WIP)

An OpenAI-compatible CLI frontend for any LLM provider, with agentic tool capabilities, written in Rust. Defaults to OpenRouter, but works with any OpenAI-compatible service (OrcaRouter, Together, Groq, self-hosted gateways, etc) via `clank endpoint` — see [Using other providers](#using-other-providers).

CCC is most stable on Linux at the moment!

## Features

- **Fast & lightweight** — Compiled Rust binary, single executable with no runtime dependencies
- **Multiple interaction modes** — Q&A, interactive chat, agentic tasks, and a full-screen TUI
- **Streaming responses** — Replies appear as they're generated rather than all at once
- **File operations** — LLM can read, write, and modify local files
- **Model selection** — Choose from configured provider's models
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

The binary will be at `target/release/clank` (or `clank.exe` on Windows).

### Install Globally

```bash
cargo install --path .
```

Then use `clank` from anywhere:

```bash
clank login
clank ask "Hello"
```

## Usage

### Commands

#### `login`
Set up or update your API key. The key is stored in your OS keychain (macOS Keychain, Windows Credential Manager, or the Linux Secret Service) rather than in a plaintext config file.

```bash
clank login
```

#### `logout`
Remove your stored API key from the OS keychain.

```bash
clank logout
```

#### `status`
Check your configuration.

```bash
clank status
```

#### `models`
List available models from your configured provider (shows first 20).

```bash
clank models
```

#### `model [name]`
View or set the persistent default model, so you don't need to pass `-m` on every call.

```bash
# Show the current default
clank model

# Set the default model
clank model anthropic/claude-opus-4.5

# Clear the default (falls back to openrouter/auto)
clank model --clear
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
clank max-iterations

# Set the default
clank max-iterations 20

# Clear it — agent mode then needs a cap set per call (--max-iterations) or
# per session (/max-iterations) to run at all; it does not fall back to 20
clank max-iterations --clear
```

Ships at 20 on a fresh install (no `config.json` yet), but once cleared it
stays cleared — nothing silently reintroduces a number. Overridden per call
with `--max-iterations` on `agent`, or persistently per session with
`/max-iterations` inside a `session`/`tui` conversation — see
[Per-session models](#model-name) for how that precedence works.

#### `temperature [value]`
View or set the persistent default sampling temperature (0-2) sent to models that support it.

```bash
# Show the current default
clank temperature

# Set the default
clank temperature 1.2

# Clear it — requests are then sent with no temperature field at all, and
# the provider uses its own default, rather than this falling back to 0.7
clank temperature --clear
```

Ships at 0.7 on a fresh install, same caveat as `max-iterations` once
cleared. Overridden per call with `--temperature` on `ask`, `session`, or
`agent`, or persistently per session with `/temperature` (or its `/temp`
shorthand) inside a `session`/`tui` conversation — see [Per-session
models](#model-name) for how that precedence works.

In `tui`, the session's current value shows in the settings row as 🌡
`<value>` (or `default` when nullified — see [Session
Persistence](#session-persistence)), color-coded cool-to-hot (cyan → yellow
→ orange → pink) as it rises from 0.

#### `verbose [on|off]`
View or set whether new sessions start showing full tool-call detail — arguments, results, and the model's own thinking.

```bash
# Show the current setting
clank verbose

# Start new sessions verbose
clank verbose on
```

Off by default. This is the *starting* value: a session snapshots it at creation, and `/verbose` from then on toggles that session, which is remembered per session rather than changing this. `clank agent -v` is unaffected — it's a per-run flag.

#### `highlight [on|off]`
View or set whether new sessions band your own messages in the transcript, so
they stand out when scrolling back through a long turn.

```bash
# Show the current setting
clank highlight

# Start new sessions without the band
clank highlight off
```

On by default, and the *starting* value like `verbose`: a session snapshots it
at creation, and `/highlight` changes that one session from then on.

The band is derived from your terminal's own background — one faint step
lighter on a dark theme, darker on a light one (NOT working too well atm) — rather than a fixed colour, so
it stays a tint of whatever you're using rather than a bar drawn over it. If
your terminal doesn't answer the query that asks, no band is drawn at all
rather than one guessed at.

#### `selection [on|off]`
View or set whether the launch screen bands its selected row.

```bash
clank selection off
```

On by default. Global only, with no per-session counterpart and no slash
command: the launch screen belongs to no session, so there is nothing to
override it with.

#### `sandbox [on|off]`
View or set whether the agent's file-writing tools are confined to your current working directory.

```bash
# Show the current setting
clank sandbox

# Let the agent write anywhere it has permission to
clank sandbox off
```

On by default, and the bound is the working directory alone — not your home directory, which would let an agent write across every project you keep under `~`. It bounds `write_file` and `replace_in_file` only: reads are never restricted, since they change nothing and confining them would break ordinary work like reading a file under `/etc`. The bound is checked against the path a write *resolves to*, so `..` and symlinks can't be used to step outside it, and `clank` writes its own `~/.clank` state directly so that keeps working at any setting.

This is the persistent default; a session snapshots it at creation, and `/sandbox` changes the session you're in. It's a separate axis from `approval`: approval decides whether you're *asked* first, the sandbox decides whether the write is allowed at all.

#### `effort [value]`
View or set the persistent default reasoning effort sent to models that support it. Applies to `ask`, `session`, and `agent`. Usually `low`, `medium`, or `high`, but not checked against a fixed list — models vary in what they accept, and an unsupported value just gets rejected by the API.

```bash
# Show the current effort level
clank effort

# Set the default
clank effort high

# Clear it (falls back to the provider default)
clank effort --clear
```

Overridden per call with `--effort-level` on `ask`, `session`, or `agent`, or
persistently per session with `/effort` inside a `session`/`tui` conversation.

When an effort level is set, `ask`, `session`, and `agent` label responses as `<model> (<effort>)` instead of just `<model>`, so you can see which effort level produced a given answer.

In `tui`, the session's current value shows in the settings row as 🧠
`<level>`, color-coded calm-to-intense (cyan → yellow → red) as it rises
from `low`.

#### `endpoint [url]`
View or set the API base URL, so you can point `clank` at any OpenAI-compatible service instead of OpenRouter (OrcaRouter, Together, Groq, a self-hosted gateway, etc).

```bash
# Show the current endpoint
clank endpoint

# Point at OrcaRouter
clank endpoint https://api.orcarouter.ai/v1

# Clear it (falls back to the OpenRouter default)
clank endpoint --clear
```

Switching endpoints doesn't switch your API key or default model automatically — run `clank login` to set the new provider's key, and `clank model` to set a model it actually serves.

#### `effort-style [value]`
View or set how the reasoning effort level (`clank effort`) is serialized in requests, since providers disagree on the shape:

- `nested` (default) — sends `reasoning: { effort: "<level>" }`, as OpenRouter expects.
- `flat` — sends `reasoning_effort: "<level>"` at the top level, as OrcaRouter expects.
- `none` — omits effort entirely, for providers that reject unrecognized fields.

```bash
clank effort-style
clank effort-style flat
clank effort-style --clear
```

#### `headers`
View or manage extra HTTP headers sent with every API request, useful for providers with optional attribution headers (e.g. OpenRouter's `HTTP-Referer`/`X-Title`).

```bash
# Show current extra headers
clank headers

# Set a header
clank headers set HTTP-Referer https://myapp.example.com
clank headers set X-Title "My App"

# Remove one
clank headers unset HTTP-Referer
```

#### `approval`
Configure approval settings for agentic actions. By default, the agent prompts for approval before reading files, writing files, or running terminal commands.

```bash
# Show current approval settings
clank approval

# Configure individual approvals (use on/off, true/false, yes/no, or 1/0)
clank approval read off      # Auto-approve file reads
clank approval write on      # Prompt before file writes
clank approval terminal on   # Prompt before terminal commands

# Set all approvals at once
clank approval all off       # Auto-approve everything (use with caution)
clank approval all on        # Prompt for all actions
```

**Per-session approval.** These commands set the default new sessions start
with. A session remembers its own approval settings too: running `/approval`
inside it (see [`session`](#session) or [the full-screen
UI](#clank-with-no-command)) switches and records
them for that session alone, the same way `/model` does for models — so
resuming later with no override picks up where you left off rather than
reverting to the configured default.

#### `ask <prompt>`
Send a single prompt to the LLM.

```bash
clank ask "What's the capital of France?"

# Specify a model
clank ask "Explain quantum computing" -m anthropic/claude-opus-4.5

# Override temperature or effort level for this call only
clank ask "Write a haiku" --temperature 1.2
clank ask "Design a lock-free queue" --effort-level high
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
| `/help` | List every in-session command and what it does. The same list in both front ends, generated from the one the parser uses, so it cannot drift from what actually works |
| `/model <name>` | Switch the model for the rest of the session, and remember it |
| `/model` | Show the model currently in use |
| `/agent` | Turn on tool-calling for the rest of the session |
| `/ask` | Turn tool-calling back off |
| `/effort` | Show the reasoning effort level currently in use |
| `/effort <level>` | Switch reasoning effort for the rest of the session, and remember it |
| `/effort clear` | Nullify it — no effort field is sent at all until set again |
| `/effort default` | Read the *currently* configured default effort and save that to the session |
| `/verbose <on\|off>` | Show the model's thinking and full tool call arguments/results, or a one-line notice per call. Bare `/verbose` shows the current setting |
| `/stream <on\|off>` | Stream this session's replies token-by-token, or wait for the whole reply. Bare `/stream` shows the current setting. Overrides `clank stream` for this session |
| `/max-iterations <n>` | Switch the tool-calling iteration cap per turn (agent mode only), and remember it |
| `/max-iterations clear` | Nullify it — agent mode then errors on any turn until a cap is set again |
| `/max-iterations default` | Read the *currently* configured default cap and save that to the session |
| `/temperature <n>` (or `/temp <n>`) | Switch the sampling temperature for the rest of the session, and remember it |
| `/temperature clear` (or `/temp clear`) | Nullify it — requests are then sent with no temperature field |
| `/temperature default` (or `/temp default`) | Read the *currently* configured default temperature and save that to the session |
| `/temperature` (or `/temp`) | Show the temperature currently in use |
| `/approval <read\|write\|terminal\|all> <on\|off>` | Switch a tool-approval gate for the rest of the session, and remember it. Takes effect immediately — including partway through a running turn, from its next tool call |
| `/approval` | Show the approval gates currently in use |
| `/sandbox <on\|off>` | Confine the agent's file writes to the working directory, or allow them anywhere. Takes effect immediately, including partway through a running turn |
| `/sandbox` | Show whether writes are currently confined |
| `/status` | Show every setting this session is running with — model, mode, effort, temperature, iteration cap, sandbox, verbose, highlighting, streaming, approval gates, and the directory it runs in. The session-scoped counterpart to `clank status` |
| `/highlight <on\|off>` | Band your own messages in the transcript, or don't. Bare `/highlight` shows the current setting |
| `/session title <new title>` | Rename this session. Bare `/session` (or `/session title`) shows its current name |
| `/send`, `/discard` | Answer the `$` command box — the same as `Ctrl-S` and `Ctrl-D`. Typed forms exist because terminals claim chords: Zed's takes `Ctrl-S` |
| `/allow`, `/deny` | Answer a tool approval — the same as `Ctrl-Y` and `Ctrl-N`. Without a way to answer, a turn waits on a decision it can never be given |
| `/back` | Return to the launch screen — the same as `Ctrl-B`, which tmux claims as its own prefix |

A mistyped invocation of one of these (`/effort` with no value, `/approval
bogus off`), or a misspelled command name (`/mode` for `/model`), is
reported as an error rather than sent to the model — see the note under
`tui`'s command table for the exact boundary.

A new session needs a name. Pass one with `--title`, or you'll be asked for it before the session starts — starting one is meant to be deliberate, so there's no untitled path and a blank answer is refused. A resumed session keeps the name it has, and `--title` is ignored with a note.

```bash
clank session --title "Fix the parser"
# Type exit to quit

# Omit --title and you'll be prompted for one
clank session

# Override the default model for a new session (ignored when resuming —
# a resumed session always keeps its own saved model)
clank session -m anthropic/claude-opus-4.5

# Override the default max tool-calling iterations per turn while in agent mode
clank session --max-iterations 30

# Override the default reasoning effort for a new session (ignored when
# resuming — a resumed session always keeps its own saved value)
clank session --effort-level high

# Resume a previous session by id (or a unique prefix of it) — works
# whether that session is currently in ask or agent mode
clank session --resume a1b2c3d4

# Or omit the id to pick from a numbered list of all your saved sessions
clank session --resume
```

#### `agent <task>`
Run a single agentic task where the LLM can use tools (read/write files) — one-shot, not a persistent session. For a continuous conversation with tools, use `session` and `/agent`.

```bash
# Create a new file
clank agent "Create a file called hello.rs that prints 'Hello, world!'"

# Modify existing code
clank agent "Read src/main.rs, identify improvements, and write an optimized version"

# Show detailed iteration logs
clank agent "Create utils.rs with a reverse array function" -v

# Override the default max iterations for this call
clank agent "Generate project structure" --max-iterations 30

# Override temperature or effort level for this call only
clank agent "Generate project structure" --temperature 0.3
clank agent "Design a lock-free queue" --effort-level high
```

##### Saving a run as a session

By default `agent` is one-shot: it does the work and leaves nothing behind.
`--session` saves it instead, so it appears in the picker and in `clank
sessions`, reports `working`/`failed`/`replied` while it runs, and can be
reopened later with `clank session --resume <id>` or `tui`. The session is
named after the task, and the id is printed when it starts.

Only one process may run a session at a time. A run claims the session before
it reads its history, and a second process is refused rather than interleaving
two sets of turns into a history neither of them wrote. The claim expires by
itself, so a session whose runner died is available again with nothing to
clean up.

#### `clank` with no command
A full-screen terminal UI. Unlike the line-based `session`, it owns the
screen, which is what lets the input box stay live while a reply streams in,
tool approvals appear inline, and a running turn be interrupted. Otherwise
the two are functionally identical — same commands, same settings, same
saved sessions, interchangeably resumable from either.

It's not a subcommand — there are no flags. Run `clank` with nothing else on
the command line, and it opens on a **launch screen**: start a new session,
or pick up any saved one. Sessions are grouped by where they live — the ones
started in your current directory first, then everything else — and each row
shows its directory, since that's where it will resume and what its sandbox
will be bounded to.

```bash
clank
```

Every new session starts in plain ask mode; use `/agent` from inside it to
turn tools on (see **Commands** below) — there's no separate "agent" launch
option. A resumed session picks back up in whichever mode, model, and effort
level it was last left in.

Choosing "New session" from the launch screen asks for a title, and requires
one — starting a session is meant to be deliberate, so there's no untitled
path. The session is kept from the moment you confirm it, whether or not you
ever say anything in it.

Every session carries a small square of braille dots, hashed from its id and
the same for the life of the session. It says nothing you can type — the
session id isn't shown on the launch screen at all — and exists purely so a
row you have seen before is recognisable while the list refreshes and rows
move under you. The same mark is the gutter glyph beside every reply once
you're inside that session, and the CLI's `session` and `agent` draw it too,
so a reply is tied to the session it came from wherever you read it.

A session being run by another process can be seen but not opened: only one
process may hold a session at a time, since two appending turns to one
history would interleave them irreparably. Opening one says so. The hold is
released when that process finishes, and expires by itself within half a
minute if it died instead.

Each row also shows what that session is doing, re-read every couple of
seconds so one you're running in another terminal stays current:

| | Meaning |
|---|---|
| spinner, yellow | Working — a request is in flight right now. The same animation and colour a conversation shows for itself |
| `?` yellow | Waiting on an approval nobody has answered |
| `✗` red | The last turn ended in an error — worth resuming to see why |
| `✓` green | The model answered; the turn ran to completion |
| `⎚` grey | Held by another process — it can be seen but not opened. Only shown when nothing else already implies a live process: a working or waiting session says so with its own badge |
| `⋯` cyan | Something was sent and nothing came back, and no process is saying otherwise |
| `⚑` yellow | Stopped part-way — after a tool result with no answer, or on a tool call that never ran |
| (blank) | Created, never used |

…followed by a one-line preview: normally the session's last message (a tool
call shows the tool it asked for), but a session waiting on an approval shows
*what* it's asking about instead — `needs approval — run_terminal_command: rm
-rf build` — since that's the row you'd want to act on.

The first three come from the process running the session, which is the only
thing that knows them: a turn's messages are only written when it *finishes*,
so from storage alone a request in flight looks exactly like a turn that
failed. The rest are read from the messages themselves, and are what a
session nobody is running can tell you.

Sessions started or deleted elsewhere appear and disappear as the list
refreshes, and the cursor follows the session it was on rather than the row
number, so rows moving underneath it can't quietly select a different
conversation.

A process killed outright leaves its last word behind until something opens
that session again.

**Launch screen**

| Key | Does |
|---|---|
| `↑` / `↓` (or `k` / `j`) | Move the selection (section labels are skipped) |
| `Enter` | Open the selected row |
| `r` | Rename the selected session |
| `d` | Delete the selected session (asks to confirm) |
| `q` | Quit |

Opening a session whose directory no longer exists asks whether to resume in
the current one instead, repointing the session there — the same thing
`clank session --resume <id> --here` does from the shell.

**In a conversation**

| Key | Does |
|---|---|
| `Enter` | Send. If a turn is already running, see **Sending while a turn is running** below — in agent mode the message joins that turn; in ask mode it waits and becomes the next one |
| `Esc` | Cancel the in-flight turn (kills a running tool command too) |
| `Alt-Enter` / `Shift-Enter` | Insert a newline instead of sending. `Alt-Enter` works everywhere; `Shift-Enter` needs a terminal that supports the kitty keyboard protocol (kitty, WezTerm, Ghostty, foot, recent Alacritty), because the older input protocol can't tell `Shift-Enter` apart from `Enter` at all |
| `↑` / `↓` | Recall previous messages into the input box |
| `$ <command>` | Run a shell command yourself, in the session's directory. No model call, no tokens, no approval — you typed it. Output appears in its own box and waits for you to decide whether the model should see it |
| `Ctrl-S` / `Ctrl-D` | Send that output to the conversation, or discard it. Sending waits for your next message rather than prompting a reply. Different keys from the approval's on purpose, since both boxes can be open at once. Use `/send` and `/discard` where something has claimed the chord — Zed's terminal does |
| `Ctrl-Y` / `Ctrl-N` | Allow or deny a tool approval. It gets its own box above the prompt rather than taking the prompt over, so you can keep typing — and keep sending — while a decision waits, which is why it's a chord rather than a bare `y`/`n`. Use `/allow` and `/deny` where the chords are claimed; without a way to answer, a turn waits forever |
| `PgUp` / `PgDn` / `End` | Scroll the transcript; `End` re-pins to the newest |
| Mouse wheel | Also scrolls the transcript — `↑`/`↓` stay dedicated to prompt history |
| `Ctrl-Shift-V` / `Shift-Insert` / middle-click | Paste, using your terminal's own paste binding. Multi-line pastes land in the input box as text rather than sending a message per line. `Ctrl-V` is **not** a paste key in most terminals — it never reaches your clipboard |
| `Ctrl-B` | Back to the launch screen (the session is saved). `/back` does the same — tmux takes `Ctrl-B` as its own prefix |
| `Ctrl-C` | Quit |

**Commands.** Type these in the message box instead of a message:

| Command | Does |
|---|---|
| `/help` | List every in-session command and what it does. The same list in both front ends, generated from the one the parser uses, so it cannot drift from what actually works |
| `/model <name>` | Switch the model for the rest of the session, and remember it |
| `/model` | Show the model currently in use |
| `/agent` | Turn on tool-calling (read/write files, run commands) for the rest of the session |
| `/ask` | Turn tool-calling back off |
| `/effort` | Show the reasoning effort level currently in use |
| `/effort <level>` | Switch reasoning effort for the rest of the session, and remember it |
| `/effort clear` | Nullify it — no effort field is sent at all until set again |
| `/effort default` | Read the *currently* configured default effort and save that to the session |
| `/verbose <on\|off>` | Show the model's thinking and full tool call arguments/results, or a one-line notice per call. Bare `/verbose` shows the current setting |
| `/stream <on\|off>` | Stream this session's replies token-by-token, or wait for the whole reply. Bare `/stream` shows the current setting. Overrides `clank stream` for this session |
| `/max-iterations <n>` | Switch the tool-calling iteration cap per turn (agent mode only), and remember it |
| `/max-iterations clear` | Nullify it — agent mode then errors on any turn until a cap is set again |
| `/max-iterations default` | Read the *currently* configured default cap and save that to the session |
| `/temperature <n>` (or `/temp <n>`) | Switch the sampling temperature for the rest of the session, and remember it |
| `/temperature clear` (or `/temp clear`) | Nullify it — requests are then sent with no temperature field |
| `/temperature default` (or `/temp default`) | Read the *currently* configured default temperature and save that to the session |
| `/temperature` (or `/temp`) | Show the temperature currently in use |
| `/approval <read\|write\|terminal\|all> <on\|off>` | Switch a tool-approval gate for the rest of the session, and remember it. Takes effect immediately — including partway through a running turn, from its next tool call |
| `/approval` | Show the approval gates currently in use |
| `/sandbox <on\|off>` | Confine the agent's file writes to the working directory, or allow them anywhere. Takes effect immediately, including partway through a running turn |
| `/sandbox` | Show whether writes are currently confined |
| `/status` | Show every setting this session is running with — model, mode, effort, temperature, iteration cap, sandbox, verbose, highlighting, streaming, approval gates, and the directory it runs in. The session-scoped counterpart to `clank status` |
| `/highlight <on\|off>` | Band your own messages in the transcript, or don't. Bare `/highlight` shows the current setting |
| `/session title <new title>` | Rename this session. Bare `/session` (or `/session title`) shows its current name |

Only recognized commands are intercepted — including a *mistyped* one.
`/approval bogus off`, or a bare `/effort` with no value, is reported as an
error rather than sent to the model, since a line naming a known command is
confidently meant as one. So is a misspelled command name: `/mode gpt-5`
answers with `Did you mean /model?` instead of quietly asking the model
about it.

A message that merely starts with a slash is still sent as normal text
whenever it isn't close to a command — paths (`/etc/hosts`), and words that
merely extend a command name (`/verbosely`), both go through untouched.

All of the above persist to the session, so they stick across
`Ctrl-B`/`--resume` too.

A session records the directory it was started in. Resuming moves the process back into it, because that directory is the sandbox's boundary and what the session's relative paths resolve against — resuming somewhere else would silently rebind both to wherever your shell happened to be. If the directory no longer exists, resuming stops and says so — the CLI with an error, the TUI by asking whether to resume here instead and repoint the session. `clank session --resume <id> --here` resumes in the current directory and repoints the session, for a project that moved. Sessions saved before this was tracked have no recorded directory and resume wherever they're run, as they always did.

Sessions are saved exactly as the other commands save them, so a `tui`
session can be resumed with `clank session --resume` and vice versa — mode,
model, and effort level all carry over either way. A session is kept from the
moment you name it, whether or not anything is ever said in it — naming one
is the deliberate act of starting it. (Sessions created before names were
required can still be untitled; one of those is discarded if you open it and
leave without saying anything, rather than leaving an empty "Untitled" in
your list.)

#### `stream [on|off]`
Whether replies stream in as they're generated. On by default. Turn it off for
providers that handle streaming — particularly streaming alongside tool calls —
badly; the CLI then waits for the whole reply as it used to.

```bash
clank stream          # show the current setting
clank stream off      # wait for complete replies
clank stream on
```

#### `timeout [name] [seconds]`
How long the client waits, in four places. Bare `clank timeout` shows them all.

```bash
clank timeout                      # show all four
clank timeout stream-idle          # show one
clank timeout stream-idle 180      # set it
```

| | Default | Bounds |
|---|---|---|
| `connect` | 20s | Connecting: DNS, TCP and TLS. Independent of how long the provider then takes to answer |
| `request` | 300s | A whole non-streaming reply. It has no partial progress to show, so it gets one generous ceiling |
| `stream-idle` | 90s | The gap *between* streamed chunks. A long reply legitimately keeps sending, so there is no total ceiling — this catches a connection that has stalled rather than a model still thinking |
| `command` | 30s | A terminal command the agent runs, when the model names no `timeout_secs` of its own |

`stream-idle` is the one worth raising behind a slow provider: it is what
ends a turn that was still coming. A timeout of `0` is refused, since it
would fail every call before it started.

#### `sessions`
List, inspect, or delete saved `session`/`tui` sessions.

```bash
# List all saved sessions (id prefix, kind, state, model, title)
clank sessions list
#   a1b2c3d4  [agent]  working   openrouter/auto  Fix the Windows build
#             run_terminal_command: cargo test --all
#   b2c3d4e5  [ask]    replied   openrouter/auto  Notes on the picker

# Show a session's full message history
clank sessions show a1b2c3d4

# Delete a saved session
clank sessions delete a1b2c3d4
```

The state column is the same one the launch screen shows, from the same
derivation: `working`, `approval`, `failed`, `stopped`, `replied`, `no reply`
or `new`. A session waiting on an approval also prints what it is asking
about on the line beneath. Without the TUI's launch screen this is the only
way to see a session running in another terminal.

The first three come from the process running the session, which is the only
thing that knows them — a turn's messages are written when it *finishes*, so
from storage alone a request in flight looks exactly like a turn that failed.

Those three are only believed while that process is still there to back them
up. A running session re-stamps a heartbeat every few seconds; if the stamps
stop for longer than half a minute, whatever it last claimed is ignored and
the state is read from the messages instead. That is what stops a detached run
killed by a `kill -9`, an OOM or a reboot from leaving a row that insists it is
`working` for ever — it settles to `no reply`, which is the truth. The window
is deliberately several heartbeats wide: a briefly starved process is not a
dead one, and calling a live run dead is the worse mistake.

## Concepts

Five nouns do most of the work in this codebase, and they nest. Getting them
straight explains why some settings take effect immediately and others wait,
why a running turn is invisible in the database, and where a message typed
mid-turn can legally go.

### The ladder

**Message** — the atom. A role (`user`, `assistant`, `tool`), content, and
optionally `tool_calls` or the `tool_call_id` answering one. Stored as a row
per message, ordered by `seq` within its session.

**Request** — one HTTP POST to `/chat/completions`. Stateless, which is the
load-bearing part: it carries the *entire* message array every time, because
the provider remembers nothing between calls. This is the unit that gets
billed, times out, and rejects malformed message shapes.

**Iteration** — one lap of the agent loop: a request, plus running whatever
tools it asked for, plus appending those results to the array. Capped by
`max-iterations`. Agent mode only — ask mode makes a single request and has
no loop.

**Turn** — one thing you typed, through to a final answer. One request in ask
mode; one to `max-iterations` iterations in agent mode, ending when the model
stops asking for tools, the cap is reached, or you cancel. A turn is also the
persistence unit: its messages are written when it *finishes*, which is why a
turn in progress leaves no trace in the messages table and why sessions carry
a separate `activity` column for the picker to read.

**Session** — a saved conversation: its messages, plus a title, a model, a
mode, a settings snapshot, and the directory it belongs to. Survives exit,
resumes by id.

So messages make up a request, requests make up an iteration, iterations make
up a turn, and turns make up a session.

### Alongside them

**Conversation** — the runtime that drives a session: it takes commands,
emits events, and runs the agent loop on its own task so the interface stays
responsive while a turn works. The session is the state; the conversation is
the thing moving it. It exists only while the process runs, and nothing stops
two processes from driving one session — see the note in TODO.

**Activity** — the only thing stored about a turn that hasn't finished:
working, awaiting approval, failed, or null for "nothing to say, read the
messages."

### Running a command yourself

`$ cargo test` runs it here and now, in the session's directory, without
involving the model. There is no approval prompt: you typed it, so there is
nothing to approve.

A box appears as soon as you press Enter. The command sits on its first line
with a spinner after it while it runs, and its output fills in underneath:

```
┌──────────────────────────────────────────────────┐
│$ cargo test ⠹                                    │
└──────────────────────────────────────────────────┘

┌ Ctrl-S send with next message · Ctrl-D discard ──┐
│$ cargo test                                      │
│running 319 tests                                 │
│test result: ok. 319 passed; 0 failed             │
└──────────────────────────────────────────────────┘
```

A non-zero exit shows in red beside the command. The border carries only what
you can act on.

`Ctrl-S` puts the output into the conversation; `Ctrl-D` leaves it out. Either
way the command stays in the transcript, marked sent or not — the decision is
about what the *model* sees, not what you see.

**Sending does not prompt a reply.** The output is added and waits, so it
reaches the model together with whatever you type next. That is the point of
the feature: `$ cargo test`, send, "fix these failures" is one turn, where
replying to bare output first would cost two. Sending during a turn joins that
turn, the same as any message.

Running a second command before answering the first drops that output from the
conversation, but it stays in the transcript marked `not sent` — nothing you
were deciding about disappears without a trace. A second command *while* one is
still running is refused: there is one box, and two results would land on top
of each other.

`Ctrl-S` was historically XOFF, the key that freezes a terminal until
`Ctrl-Q`. It works here because raw mode turns software flow control off — but
anything layered above the terminal can still claim it, which is what `/send`
and `/discard` are for. If a terminal ever does lock up, `Ctrl-Q` releases it.

**Commands get no stdin.** Anything that wants input — `sudo` without a
cached credential, `git commit` with no `-m`, a script calling `read` — gets
end-of-file immediately and fails with its own error, rather than blocking
until it is killed. Use another terminal for anything interactive.

Output is capped, keeping the end rather than the beginning, since a failing
build says what went wrong on its last lines. Commands are killed after 30
seconds, and `$` is TUI-only for now.

### Sending while a turn is running

You do not have to wait for a turn to finish before typing. What happens next
depends on whether there is a loop to join.

In **agent mode**, the message joins the turn already running. A turn is many
requests, and the message array for each one is built fresh, so the loop takes
whatever you have typed at the top of its next iteration and includes it in
that request. The model sees it before deciding what to do next, which means
you can redirect work in progress — "actually, skip the tests", "check the
Windows path too" — rather than waiting for it to finish and correcting it
afterwards.

The timing is bounded by what the turn is doing. If a tool call is running,
your message lands as soon as that call finishes and the loop comes back
around, because that is the first legal place to put it: a message carrying
tool calls must be followed by their results, and nothing may come between.
It never interrupts a request already in flight — there is no such thing in
an OpenAI-compatible API.

In **ask mode** there is one request and no loop, so there is no seam to
inject into. The message waits and becomes its own turn when the current one
finishes. The same fallback applies in agent mode if the turn ends before the
loop takes what you typed — the iteration cap was reached, the turn failed,
or you cancelled.

A box appears above the message input as soon as something is waiting, and
disappears when the last one leaves. It lists the messages in the order they
will be taken, and its title says where they are headed — `joining this turn`
in agent mode, `next turn` in ask mode:

```
╭─ joining this turn ────────────────────────╮
│check the Windows path too                  │
│and skip the slow tests                     │
╰────────────────────────────────────────────╯
╭────────────────────────────────────────────╮
│                                            │
╰────────────────────────────────────────────╯
 ⠋ working · agent · sonnet-5 · 🧠 high
```

Each message drops out of the box as it is consumed and appears in the
transcript at the point it actually joined, so the conversation reads in the
order the model saw it. Past five waiting, the rest are summarised as
`+N more`. Cancelling a turn with `Esc` drops anything still waiting.

### Where a setting lives

| Scope | Changed by | Read |
|---|---|---|
| Global default | `clank <setting> <value>` | when a session is created |
| Session | `/model`, `/temperature`, … | stored on the session row |
| Per-turn snapshot | — | once, when a turn starts |
| Live gates | `/approval`, `/sandbox` | before every tool call |

The last two rows are the distinction worth knowing. Model, effort,
temperature and streaming are snapshotted when a turn starts, so changing one
mid-turn applies to the *next* turn. Approval and sandbox are re-read before
every single tool call, so changing one mid-turn applies to the turn that is
running — which is the entire point, since revoking permission is not much
use if it waits politely for the current work to finish.

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

### `web_fetch`
Fetch an `http`/`https` URL and return the page as readable text rather than HTML.

The agent can already reach the web through `run_terminal_command` — it can run
`curl`. This exists because the raw page is mostly markup: converting first cuts
a documentation page to between a half and a quarter of its size (measured: 4.0×
on docs.rs, 3.8× on MDN, 2.0× on the Rust book), and whatever is fetched stays in
the conversation for the rest of the turn.

**It does not ask for approval.** It reads a page and changes nothing, and a
prompt on every page is the friction that would send the model back to curling
raw markup through `run_terminal_command` — which does prompt, and can then do
anything. There is no `/approval` category for it.

Refuses anything that isn't `http` or `https` (notably `file:`, which would read
the disk through a tool the sandbox doesn't cover), refuses content types it
can't read as text, caps a page at 1 MB, and times out after 30 seconds. What
comes back is labelled untrusted: it is the only tool result that originates
neither with you nor with your machine, so a page telling the agent to run
something is an attack, and the approval prompt is what stands in the way.

## Configuration

Configuration is stored at `~/.clank/config.json`:

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
  "extra_headers": {},
  "sandbox": true,
  "verbose": false,
  "highlight": true,
  "selection": true,
  "stream": true
}
```

- The file is created the first time you change a setting, not on first run — until then every value comes from the defaults above, which `clank status` will show you. You can also write it by hand: any keys you leave out fall back to their defaults, so a file containing only `{"temperature": 1.5}` is valid, and the next `clank` setting command fills in the rest around what you wrote.
- If the file can't be parsed, commands stop with the parse position rather than silently reverting to defaults, and nothing is written over it — a malformed config would otherwise send your API key to the default endpoint instead of the one you configured, and the next setting command would overwrite everything else you'd set. Fix it, or delete it to start over.
- Your API key is **not** in this file — `clank login`/`logout` store and remove it from the OS keychain instead (see [Security](#security)). If you have an old config with a plaintext `api_key` field, the next command that loads config transparently migrates it into the OS keychain and rewrites the file without it.
- `base_url` is managed via `clank endpoint` and is the API endpoint used by every command. Defaults to OpenRouter; point it at any OpenAI-compatible service.
- `default_model` is managed via `clank model` and is used by `ask`, `session`, and `agent` when `-m`/`--model` isn't passed, and always by `tui`, which has no flags at all.
- `approval` settings control whether the agent prompts before performing actions. Managed via `clank approval`.
- `max_iterations` is managed via `clank max-iterations` and is the default for `session`/`agent` when `--max-iterations` isn't passed, and for `tui`, which has no flags at all. `null` (after `clank max-iterations --clear`) means agent mode has no cap until one is set somewhere — it does not fall back to 20.
- `temperature` is managed via `clank temperature` and is the default for `ask`, `session`, and `agent` when `--temperature` isn't passed, and for `tui`, which has no flags at all. `null` (after `clank temperature --clear`) means requests are sent with no `temperature` field at all — it does not fall back to 0.7.
- `verbose` is managed via `clank verbose` and is the value new sessions start with; `/verbose` changes the session you're in, not this.
- `highlight` is managed via `clank highlight` and is the value new sessions start with for banding your own messages; `/highlight` changes the session you're in, not this.
- `selection` is managed via `clank selection` and controls the band on the launch screen's selected row. Global only — that screen belongs to no session, so there is no per-session counterpart and no slash command.
- `effort_level` is managed via `clank effort-level` and is sent for `ask`, `session`, and `agent` when set, shaped according to `effort_style`.
- `effort_style` is managed via `clank effort-style` and controls whether the effort level is sent flat, nested, or omitted (see [`effort-style`](#effort-style-value)).
- `extra_headers` is managed via `clank headers` and is merged into every API request.

### Using other providers

Clanker Command Center talks to any service exposing an OpenAI-compatible `/chat/completions` and `/models` API over `Authorization: Bearer` auth — this covers OpenRouter, OrcaRouter, Together, Groq, Fireworks, and self-hosted gateways (vLLM, Ollama's OpenAI shim, LM Studio). It does not cover providers with a different auth scheme or URL shape, like Azure OpenAI.

To switch to OrcaRouter, for example:

```bash
clank endpoint https://api.orcarouter.ai/v1
clank login                          # enter your OrcaRouter key
clank model orcarouter/auto          # or any model OrcaRouter serves
clank effort-style flat              # OrcaRouter expects reasoning as a top-level field
```

Only one provider is active at a time today — switching back to OpenRouter means re-running `clank endpoint`, `clank login`, `clank model`, and `clank effort-style` for it. Named provider profiles (switch between saved providers with one command) are tracked in `TODO.md`.

## Session Persistence

`session` and `tui` conversations are saved automatically to a SQLite database at `~/.clank/chats.db`. Every message (yours, the assistant's, and any tool calls/results while in agent mode) is written as the conversation happens, so you don't lose anything if you exit or your terminal closes — including a turn you cancelled partway through.

**Settings are a snapshot, not a live link to your config.** A session's row — model, effort level, max iterations, temperature, approval gates — is written to the database the moment it's created, before your first message, not after. `tui` has no flags at all, so a session it creates is always a straight snapshot of your persistent config defaults; `clank session` is the only place a brand new session can start away from those defaults, via its `--model`/`--effort-level`/`--max-iterations`/`--temperature` flags. That snapshot can itself be `None` for effort/max-iterations/temperature, if nothing is configured anywhere — same as `ask`/`agent`, which merge a `--flag` with the config default the same way but only ever for that one call, never a session.

From then on, the session's settings are entirely its own: `/model` and `/approval` changes always write a concrete value straight back to that same row; `/effort`, `/max-iterations`, and `/temperature` additionally support two different resets, since a session can also nullify these three:

- **`/setting clear`** nullifies it outright, with no fallback substituted anywhere: `/effort clear` and `/temperature clear` mean no effort/temperature field is sent in the request at all (the provider uses its own default); `/max-iterations clear` means agent mode has no cap, so any turn that actually needs one fails immediately with an error telling you to set one, rather than the loop running unbounded or guessing a number.
- **`/setting default`** is a one-time snapshot instead: it reads whatever the global default currently is and saves that concrete value to the session right now — frozen from that point on, exactly like typing the value itself, and distinct from `clear` even when the global default happens to be unset (an `/effort default` with no global default configured saves `None` explicitly, the same as `clear` would, but as a deliberate choice rather than an indefinite fallback).

Either way, every outgoing request from a session reads its own stored settings directly, never your global config — including for a value that's currently `None`. Later changing a global default with `clank model`/`clank temperature`/etc. never reaches into any session that already exists, whether that session has an explicit value, is nullified, or was created before you ever set the global default at all. The global defaults themselves work the same way: `clank max-iterations --clear`/`clank temperature --clear` null them out too (see [`max-iterations`](#max-iterations-value) and [`temperature`](#temperature-value)), and nothing brings them back except setting one explicitly again.

Each session gets an id (a UUID) and a title derived from your first message (or one you choose up front, in the TUI). Use:

- `clank sessions list` to see saved sessions (shown by 8-character id prefix, kind, state, model, and title)
- `clank sessions show <id>` to view a session's full transcript
- `clank sessions delete <id>` to remove one
- `clank session --resume <id>` to continue a saved session — works for one currently in ask mode or agent mode alike, since mode is just session state now, not a separate command
- `clank session --resume` with no id to pick one from a numbered list of all your saved sessions

Any unique prefix of a session's id works wherever a full id is expected. A
prefix matching more than one session is refused, and the candidates listed
with their titles — `sessions delete` resolves ids the same way, so guessing
between them would eventually delete the wrong conversation.

## Examples

### Generate and save code

```bash
clank agent "Write a function that calculates fibonacci numbers and save it to math.rs"
```

### Multi-file project setup

```bash
clank agent "Create a basic Rust project structure with Cargo.toml, src/main.rs, and src/lib.rs"
```

### Fix existing code

```bash
clank agent "Read main.rs, find any issues, and write a corrected version"
```

### Using different models

```bash
# Claude for code review
clank agent "Read app.rs and provide detailed code review feedback" -m anthropic/claude-opus-4.5

# GPT-4 for complex logic
clank agent "Create an algorithm to solve the traveling salesman problem" -m openai/gpt-4o

# Adaptive routing (default)
clank agent "Generate boilerplate code" -m openrouter/auto
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

Run `clank login` and enter your key from [openrouter.ai/keys](https://openrouter.ai/keys).

### "Model not found"

Run `clank models` to see available models, then use the correct model ID with `-m`.

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

- The agent's file-writing tools (`write_file`, `replace_in_file`) are confined to your current working directory by default, checked against the path a write resolves to so `..` and symlinks can't step outside it. Turn it off per session with `/sandbox off` or globally with `clank sandbox off`. Reads and terminal commands are not bounded this way — a terminal command runs whatever you approve. This gates the agent's tools only; `clank` writes its own `~/.clank` state directly and is unaffected
- API keys are stored in your OS keychain (macOS Keychain, Windows Credential Manager, or the Linux Secret Service via `keyring`), not in a plaintext file. An older `~/.clank/config.json` with a plaintext `api_key` field is migrated into the keychain automatically the next time you run any `clank` command, and the field is stripped from the file afterward
- `session`/`tui` history is stored in `~/.clank/chats.db` with message content, tool calls, reasoning, and titles encrypted at rest (AES-256-GCM, key held in your OS keychain under a separate `db_encryption_key` entry) — but the surrounding session metadata (roles, model names, effort levels, timestamps) is stored in the clear, and rows written before encryption existed stay plaintext until they're next written. The key lives in the same keychain `clank` already uses, so this protects the file at rest (backups, drive theft) rather than against someone who can run `clank` as you; avoid pasting secrets into a session if you plan to share the database file
- The last 100 LLM API errors (a non-2xx response, a stalled/dropped connection, a malformed stream) are kept at `~/.clank/errors.log`, so a confusing one can be looked back at without having to catch and copy it in the moment — plain text, one line per entry, oldest dropped as new ones come in
- Each of those entries records the shape of the request that failed — role sequence, tool-call and reasoning counts — but no message text. To capture the request itself, set `CLANK_DEBUG_REQUESTS=1`: the failing request's full JSON body is written to `~/.clank/failed-request.json` (only the most recent one, overwritten each time) and the log entry names the file. **That file contains the entire conversation verbatim** — every message, tool call and tool result — so it's off by default, and worth deleting once you're done with it

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
time clank ask "Hello"
# real    0m0.015s
```

## License

MIT

## Support

For issues with a specific provider's API itself (rate limits, billing, model availability), see that provider's own docs — e.g. [openrouter.ai/docs](https://openrouter.ai/docs) for the default OpenRouter endpoint.
