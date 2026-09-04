# Clanker Command Center - Examples

Real-world usage examples for Clanker Command Center.

## Quick Start

```bash
# 1. Install
cargo install --path .

# 2. Login
clank login

# 3. Try it
clank ask "What can you do?"
clank agent "Create a hello.rs file that prints 'Hello, Rust!'"
```

## Code Generation

### Generate a Rust utility function

```bash
clank agent "Create a file utils.rs with functions to:
- Format dates
- Calculate age from birthdate
- Validate email addresses"
```

### Generate with tests

```bash
clank agent "Create calculator.rs with add, subtract, multiply, divide functions, then create calculator_tests.rs with comprehensive tests"
```

## Code Modification

### Read and improve existing code

```bash
clank agent "Read src/main.rs and create an improved version with better error handling and documentation, save as src/main_improved.rs"
```

### Fix bugs in code

```bash
clank agent "Read src/buggy.rs, identify any bugs or inefficiencies, and write a fixed version to src/buggy_fixed.rs"
```

### Refactor for performance

```bash
clank agent "Read src/index.rs, analyze for performance improvements, and write an optimized version to src/index_optimized.rs"
```

## Project Structure

### Generate a basic Rust project

```bash
clank agent "Create a Rust project structure:
- Cargo.toml (with name and version)
- src/main.rs (entry point with main function)
- src/lib.rs (library module)
- src/utils.rs (helper functions)
- README.md (basic documentation)"
```

### Create a CLI tool skeleton

```bash
clank agent "Create a Rust CLI tool with:
- Cargo.toml (with clap dependency)
- src/main.rs (argument parsing using clap)
- src/commands.rs (command handlers)
- src/config.rs (configuration management)
- README.md (usage documentation)"
```

### Generate an async HTTP client

```bash
clank agent "Create a Rust HTTP client library:
- Cargo.toml (with reqwest and tokio dependencies)
- src/lib.rs (module setup)
- src/client.rs (HTTP client struct)
- src/models.rs (request/response models)
- examples/basic.rs (example usage)"
```

## Multi-step Workflows

### Generate and verify

```bash
clank agent "Generate a random number generator in random.rs using rand crate, then read it back and verify the file exists and has valid Rust syntax" -v
```

### Create and document

```bash
clank agent "Create a sorting.rs file with merge sort and quick sort implementations, then read it and create a SORTING.md file documenting the algorithms and complexity"
```

### Build and chain modifications

```bash
clank agent "Create math.rs with basic math functions, then read it and create stats.rs that uses math.rs functions for statistical calculations"
```

## Using Different Models

### Claude for code review

```bash
clank agent "Read src/main.rs and provide detailed Rust-specific code review feedback" -m anthropic/claude-opus-4.5
```

### GPT-4 for complex algorithms

```bash
clank agent "Create an implementation of the A* pathfinding algorithm in Rust" -m openai/gpt-4o
```

### Adaptive routing (default)

```bash
clank agent "Generate Rust boilerplate code for a web API" -m openrouter/auto
```

## Interactive Chat

### Have a Rust programming conversation

```bash
clank session
# ❯ What's the best way to handle errors in Rust?
# [response about Result and ?]
# ❯ Can you show me an example?
# [code example]
# ❯ exit
```

### Get design advice

```bash
clank session
# ❯ Should I use a trait or a struct for this?
# [design discussion]
# ❯ What about performance?
# [performance analysis]
# ❯ exit
```

## Advanced Scenarios

### Create a web server

```bash
clank agent "Create a basic Axum web server:
- Cargo.toml (with axum and tokio dependencies)
- src/main.rs (server setup with routes)
- src/handlers.rs (request handlers)
- src/models.rs (response models)"
```

### Database integration

```bash
clank agent "Create a database abstraction layer:
- Cargo.toml (with sqlx dependency)
- src/db.rs (database connection and queries)
- src/models.rs (data models)
- examples/query.rs (usage example)"
```

### Generate testing utilities

```bash
clank agent "Create test_utils.rs with helper functions for:
- Creating test fixtures
- Temporary file management
- Mock object creation
- Custom assertions"
```

### Create GitHub Actions workflow

```bash
clank agent "Create .github/workflows/ci.yml for a Rust project with:
- cargo test
- cargo clippy
- cargo fmt --check
- builds for Linux, macOS, and Windows"
```

## Performance-Focused Examples

### Optimize hot loop

```bash
clank agent "Read hot_loop.rs, identify performance bottlenecks, and rewrite using SIMD or other optimization techniques" -m openai/gpt-4o
```

### Memory-efficient data structure

```bash
clank agent "Create an efficient-memory.rs with a data structure that minimizes allocations while processing large datasets"
```

### Concurrent processing

```bash
clank agent "Create parallel.rs with a multi-threaded or async implementation for processing batches of items"
```

## Continuous Agentic Chat

### Iterate on a project across multiple prompts

```bash
clank session
❯ /agent
❯ Create a Cargo project for a CLI todo app with add/list/done commands
❯ Now add a --priority flag to the add subcommand
❯ Read src/main.rs back to me and suggest one refactor
# Type exit to quit
```

Every new session starts in plain ask mode; `/agent` turns on tool-calling
(reading/writing files, running commands) for the rest of it, `/ask` turns it
back off. Each prompt in agent mode is answered with the full agent tool
loop, and the whole conversation — including tool results — stays in context
for the next prompt, so you can build on prior turns instead of restating
everything in one `clank agent` call.

### Resume a saved session later

```bash
clank sessions list
#   a1b2c3d4  [agent]  replied   openrouter/auto  Create a Cargo project for a CLI todo app...

clank session --resume a1b2c3d4
# prints the prior transcript, then drops you back into the prompt
```

Don't remember the id? Leave `--resume` bare and pick from a list instead:

```bash
clank session --resume
# Select a session to resume:
#   1. a1b2c3d4  Create a Cargo project for a CLI todo app...
#   2. f9e8d7c6  Refactor the auth middleware
# Session number: 1
```

A resumed session picks up in whichever mode (ask or agent) it was last in
— that's just session state now, not a separate command. `session` and `tui`
conversations are saved automatically as you go, so this works even if you
closed the terminal without typing `exit`. See the [Session
Persistence](README.md#session-persistence) section of the README for
details.

## Tips & Tricks

### Use verbose mode to debug agent iterations

```bash
clank agent "Your Rust task" -v
```

### Increase iterations for complex tasks

```bash
# Override for a single call
clank agent "Complex multi-file project setup" --max-iterations 30

# Or raise the persistent default so every agent call gets more room
clank max-iterations 30

# Or, inside a session, just for that one session
clank session
❯ /max-iterations 30
```

### Skip approval prompts for a trusted task

```bash
# Auto-approve everything for a one-off agent call (use with caution)
clank approval all off
clank agent "Refactor the whole src/ directory"
clank approval all on

# Or, inside a session, just for that one session
clank session
❯ /agent
❯ /approval all off

# Check what a session's gates are currently set to
❯ /approval
```

### Set a reasoning effort level

```bash
# Push harder on reasoning for tough tasks
clank effort-level high
clank agent "Design a lock-free concurrent queue in Rust"

# Back off for quick, cheap responses
clank effort-level low

# Or start a single session at a different level, without changing the
# persistent default
clank session --effort-level high

# Or, inside a session, just for that one session
clank session
❯ /effort high
```

Once set, response labels show the effort level alongside the model, e.g. `anthropic/claude-opus-4.5 (high):`, so it's clear which effort level produced the output.

### Set a sampling temperature

```bash
# Override for a single call
clank agent "Complex multi-file project setup" --temperature 0.3

# Or raise the persistent default so every call is more consistent
clank temperature 0.3

# Or, inside a session, just for that one session (/temp also works)
clank session
❯ /temp 1.2
```

### Chain operations

```bash
clank agent "List files in src/, then read src/main.rs, then create a refactored version"
```

### Save an agent run as a session

```bash
# Without --session a task leaves nothing behind. With it, the run shows up
# in the picker and in `clank sessions`, and reopens with its whole
# transcript — tool calls included.
clank agent --session "Add doc comments to every pub fn in src/store.rs"
clank sessions

# Pick it up later; any unique prefix of the id works
clank agent --resume c63337c2 "Now add tests for what you just wrote"
```

A resumed session keeps its own saved model, temperature, iteration cap and
effort level — passing those flags prints a note and changes nothing — and
runs in the directory it was created in.

## Workflow Examples

### Building a microservice

```bash
clank agent "Create a Rust microservice with:
1. Cargo.toml (with axum, tokio, sqlx)
2. src/main.rs (server entry point)
3. src/handlers.rs (API endpoints)
4. src/db.rs (database layer)
5. src/models.rs (data structures)
6. src/config.rs (configuration)
7. README.md (setup and usage)"
```

### Setting up testing

```bash
clank agent "Create a comprehensive test setup:
1. Cargo.toml (with test dependencies)
2. src/lib.rs (library with public API)
3. src/main.rs (binary that uses lib)
4. tests/integration_test.rs (integration tests)
5. benches/bench.rs (performance benchmarks)"
```

### Create a library

```bash
clank agent "Scaffold a reusable Rust library:
1. Cargo.toml (publishable on crates.io)
2. src/lib.rs (module exports)
3. src/core.rs (core functionality)
4. src/error.rs (error types)
5. examples/usage.rs (example code)
6. README.md (documentation)
7. LICENSE (MIT license)"
```

### Generate CLI tool with subcommands

```bash
clank agent "Create a feature-rich CLI tool using clap:
1. Cargo.toml (with clap v4)
2. src/main.rs (argument parsing)
3. src/commands/ (subcommand modules)
4. src/config.rs (config management)
5. src/errors.rs (error handling)"
```
