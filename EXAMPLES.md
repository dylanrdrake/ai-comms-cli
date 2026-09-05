# Clanker Command Center - Examples

Real-world usage examples for Clanker Command Center.

## Quick Start

```bash
# 1. Install
cargo install --path .

# 2. Login
clank login

# 3. Try it
clank "What can you do?"
clank "Create a hello.rs file that prints 'Hello, Rust!'" --tools
```

## Code Generation

### Generate a Rust utility function

```bash
clank "Create a file utils.rs with functions to:
- Format dates
- Calculate age from birthdate
- Validate email addresses" --tools
```

### Generate with tests

```bash
clank "Create calculator.rs with add, subtract, multiply, divide functions, then create calculator_tests.rs with comprehensive tests" --tools
```

## Code Modification

### Read and improve existing code

```bash
clank "Read src/main.rs and create an improved version with better error handling and documentation, save as src/main_improved.rs" --tools
```

### Fix bugs in code

```bash
clank "Read src/buggy.rs, identify any bugs or inefficiencies, and write a fixed version to src/buggy_fixed.rs" --tools
```

### Refactor for performance

```bash
clank "Read src/index.rs, analyze for performance improvements, and write an optimized version to src/index_optimized.rs" --tools
```

## Project Structure

### Generate a basic Rust project

```bash
clank "Create a Rust project structure:
- Cargo.toml (with name and version)
- src/main.rs (entry point with main function)
- src/lib.rs (library module)
- src/utils.rs (helper functions)
- README.md (basic documentation)" --tools
```

### Create a CLI tool skeleton

```bash
clank "Create a Rust CLI tool with:
- Cargo.toml (with clap dependency)
- src/main.rs (argument parsing using clap)
- src/commands.rs (command handlers)
- src/config.rs (configuration management)
- README.md (usage documentation)" --tools
```

### Generate an async HTTP client

```bash
clank "Create a Rust HTTP client library:
- Cargo.toml (with reqwest and tokio dependencies)
- src/lib.rs (module setup)
- src/client.rs (HTTP client struct)
- src/models.rs (request/response models)
- examples/basic.rs (example usage)" --tools
```

## Multi-step Workflows

### Generate and verify

```bash
clank "Generate a random number generator in random.rs using rand crate, then read it back and verify the file exists and has valid Rust syntax" --tools -v
```

### Create and document

```bash
clank "Create a sorting.rs file with merge sort and quick sort implementations, then read it and create a SORTING.md file documenting the algorithms and complexity" --tools
```

### Build and chain modifications

```bash
clank "Create math.rs with basic math functions, then read it and create stats.rs that uses math.rs functions for statistical calculations" --tools
```

## Using Different Models

### Claude for code review

```bash
clank "Read src/main.rs and provide detailed Rust-specific code review feedback" --tools -m anthropic/claude-opus-4.5
```

### GPT-4 for complex algorithms

```bash
clank "Create an implementation of the A* pathfinding algorithm in Rust" --tools -m openai/gpt-4o
```

### Adaptive routing (default)

```bash
clank "Generate Rust boilerplate code for a web API" --tools -m openrouter/auto
```

## Interactive Chat

### Have a Rust programming conversation

```bash
clank clanker
# ❯ What's the best way to handle errors in Rust?
# [response about Result and ?]
# ❯ Can you show me an example?
# [code example]
# ❯ exit
```

### Get design advice

```bash
clank clanker
# ❯ Should I use a trait or a struct for this?
# [design discussion]
# ❯ What about performance?
# [performance analysis]
# ❯ exit
```

## Advanced Scenarios

### Create a web server

```bash
clank "Create a basic Axum web server:
- Cargo.toml (with axum and tokio dependencies)
- src/main.rs (server setup with routes)
- src/handlers.rs (request handlers)
- src/models.rs (response models)" --tools
```

### Database integration

```bash
clank "Create a database abstraction layer:
- Cargo.toml (with sqlx dependency)
- src/db.rs (database connection and queries)
- src/models.rs (data models)
- examples/query.rs (usage example)" --tools
```

### Generate testing utilities

```bash
clank "Create test_utils.rs with helper functions for:
- Creating test fixtures
- Temporary file management
- Mock object creation
- Custom assertions" --tools
```

### Create GitHub Actions workflow

```bash
clank "Create .github/workflows/ci.yml for a Rust project with:
- cargo test
- cargo clippy
- cargo fmt --check
- builds for Linux, macOS, and Windows" --tools
```

## Performance-Focused Examples

### Optimize hot loop

```bash
clank "Read hot_loop.rs, identify performance bottlenecks, and rewrite using SIMD or other optimization techniques" --tools -m openai/gpt-4o
```

### Memory-efficient data structure

```bash
clank "Create an efficient-memory.rs with a data structure that minimizes allocations while processing large datasets" --tools
```

### Concurrent processing

```bash
clank "Create parallel.rs with a multi-threaded or async implementation for processing batches of items" --tools
```

## Continuous Agentic Chat

### Iterate on a project across multiple prompts

```bash
clank clanker
❯ /tools on
❯ Create a Cargo project for a CLI todo app with add/list/done commands
❯ Now add a --priority flag to the add subcommand
❯ Read src/main.rs back to me and suggest one refactor
# Type exit to quit
```

A new clanker starts with no tools; `/tools on` gives it the ones `clank
tools` allows. From then on each prompt is answered with the full tool loop,
and the whole conversation — tool results included — stays in context for the
next one, so you can build on prior turns instead of restating everything in
one call.

### Resume a saved clanker later

```bash
clank clankers list
#   a1b2c3d4  [agent]  replied   openrouter/auto  Create a Cargo project for a CLI todo app...

clank clanker --resume a1b2c3d4
# prints the prior transcript, then drops you back into the prompt
```

Don't remember the id? Leave `--resume` bare and pick from a list instead:

```bash
clank clanker --resume
# Select a clanker to resume:
#   1. a1b2c3d4  Create a Cargo project for a CLI todo app...
#   2. f9e8d7c6  Refactor the auth middleware
# Clanker number: 1
```

A resumed clanker picks up in whichever mode (ask or agent) it was last in
— that's just clanker state now, not a separate command. `clanker` and `tui`
conversations are saved automatically as you go, so this works even if you
closed the terminal without typing `exit`. See the [Clanker
Persistence](README.md#clanker-persistence) section of the README for
details.

## Tips & Tricks

### Use verbose mode to debug agent iterations

```bash
clank "Your Rust task" --tools -v
```

### Increase iterations for complex tasks

```bash
# Override for a single call
clank "Complex multi-file project setup" --tools --max-iterations 30

# Or raise the persistent default so every agent call gets more room
clank max-iterations 30

# Or, inside a clanker, just for that one clanker
clank clanker
❯ /max-iterations 30
```

### Skip approval prompts for a trusted task

```bash
# Let everything run unasked for a one-off agent call (use with caution)
clank tools allow all
clank "Refactor the whole src/ directory" --tools
clank tools on              # back to the defaults

# Or, inside a clanker, just for that one clanker
clank clanker
❯ /tools allow all

# Check what a clanker's tools may do
❯ /tools
```

### Let it run shell commands

```bash
# The shell starts at `never` — not offered to the model at all, because it
# is the one tool that can do anything you can. Turn it on when you want it:
clank tools ask run_terminal_command      # asks before each command
clank tools allow run_terminal_command    # runs them unasked (careful)

# Or just for one clanker
❯ /tools ask run_terminal_command

# And back off again
clank tools never run_terminal_command
```

### Set a reasoning effort level

```bash
# Push harder on reasoning for tough tasks
clank effort-level high
clank "Design a lock-free concurrent queue in Rust" --tools

# Back off for quick, cheap responses
clank effort-level low

# Or start a single clanker at a different level, without changing the
# persistent default
clank clanker --effort-level high

# Or, inside a clanker, just for that one clanker
clank clanker
❯ /effort high
```

Once set, response labels show the effort level alongside the model, e.g. `anthropic/claude-opus-4.5 (high):`, so it's clear which effort level produced the output.

### Set a sampling temperature

```bash
# Override for a single call
clank "Complex multi-file project setup" --tools --temperature 0.3

# Or raise the persistent default so every call is more consistent
clank temperature 0.3

# Or, inside a clanker, just for that one clanker (/temp also works)
clank clanker
❯ /temp 1.2
```

### Chain operations

```bash
clank "List files in src/, then read src/main.rs, then create a refactored version" --tools
```

### Keep a run as a clanker

```bash
# Without --save a task leaves nothing behind. With it, the run shows up
# in the picker and in `clank clankers`, and reopens with its whole
# transcript — tool calls included.
clank "Add doc comments to every pub fn in src/store.rs" --tools --save
clank clankers list

# Pick it up later, in the line-based clanker or the TUI
clank clanker --resume c63337c2
```

A resumed clanker keeps its own saved model, temperature, iteration cap and
effort level, and runs in the directory it was created in.

## Workflow Examples

### Building a microservice

```bash
clank "Create a Rust microservice with:
1. Cargo.toml (with axum, tokio, sqlx)
2. src/main.rs (server entry point)
3. src/handlers.rs (API endpoints)
4. src/db.rs (database layer)
5. src/models.rs (data structures)
6. src/config.rs (configuration)
7. README.md (setup and usage)" --tools
```

### Setting up testing

```bash
clank "Create a comprehensive test setup:
1. Cargo.toml (with test dependencies)
2. src/lib.rs (library with public API)
3. src/main.rs (binary that uses lib)
4. tests/integration_test.rs (integration tests)
5. benches/bench.rs (performance benchmarks)" --tools
```

### Create a library

```bash
clank "Scaffold a reusable Rust library:
1. Cargo.toml (publishable on crates.io)
2. src/lib.rs (module exports)
3. src/core.rs (core functionality)
4. src/error.rs (error types)
5. examples/usage.rs (example code)
6. README.md (documentation)
7. LICENSE (MIT license)" --tools
```

### Generate CLI tool with subcommands

```bash
clank "Create a feature-rich CLI tool using clap:
1. Cargo.toml (with clap v4)
2. src/main.rs (argument parsing)
3. src/commands/ (subcommand modules)
4. src/config.rs (config management)
5. src/errors.rs (error handling)" --tools
```
