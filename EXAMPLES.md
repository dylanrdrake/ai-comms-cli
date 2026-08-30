# OrcaCLI Rust Edition - Examples

Real-world usage examples for the Rust version of OrcaCLI.

## Quick Start

```bash
# 1. Install
cargo install --path .

# 2. Login
orca login

# 3. Try it
orca ask "What can you do?"
orca agent "Create a hello.rs file that prints 'Hello, Rust!'"
```

## Code Generation

### Generate a Rust utility function

```bash
orca agent "Create a file utils.rs with functions to:
- Format dates
- Calculate age from birthdate
- Validate email addresses"
```

### Generate with tests

```bash
orca agent "Create calculator.rs with add, subtract, multiply, divide functions, then create calculator_tests.rs with comprehensive tests"
```

## Code Modification

### Read and improve existing code

```bash
orca agent "Read src/main.rs and create an improved version with better error handling and documentation, save as src/main_improved.rs"
```

### Fix bugs in code

```bash
orca agent "Read src/buggy.rs, identify any bugs or inefficiencies, and write a fixed version to src/buggy_fixed.rs"
```

### Refactor for performance

```bash
orca agent "Read src/index.rs, analyze for performance improvements, and write an optimized version to src/index_optimized.rs"
```

## Project Structure

### Generate a basic Rust project

```bash
orca agent "Create a Rust project structure:
- Cargo.toml (with name and version)
- src/main.rs (entry point with main function)
- src/lib.rs (library module)
- src/utils.rs (helper functions)
- README.md (basic documentation)"
```

### Create a CLI tool skeleton

```bash
orca agent "Create a Rust CLI tool with:
- Cargo.toml (with clap dependency)
- src/main.rs (argument parsing using clap)
- src/commands.rs (command handlers)
- src/config.rs (configuration management)
- README.md (usage documentation)"
```

### Generate an async HTTP client

```bash
orca agent "Create a Rust HTTP client library:
- Cargo.toml (with reqwest and tokio dependencies)
- src/lib.rs (module setup)
- src/client.rs (HTTP client struct)
- src/models.rs (request/response models)
- examples/basic.rs (example usage)"
```

## Multi-step Workflows

### Generate and verify

```bash
orca agent "Generate a random number generator in random.rs using rand crate, then read it back and verify the file exists and has valid Rust syntax" -v
```

### Create and document

```bash
orca agent "Create a sorting.rs file with merge sort and quick sort implementations, then read it and create a SORTING.md file documenting the algorithms and complexity"
```

### Build and chain modifications

```bash
orca agent "Create math.rs with basic math functions, then read it and create stats.rs that uses math.rs functions for statistical calculations"
```

## Using Different Models

### Claude for code review

```bash
orca agent "Read src/main.rs and provide detailed Rust-specific code review feedback" -m anthropic/claude-opus-4.5
```

### GPT-4 for complex algorithms

```bash
orca agent "Create an implementation of the A* pathfinding algorithm in Rust" -m openai/gpt-4o
```

### Adaptive routing (default)

```bash
orca agent "Generate Rust boilerplate code for a web API" -m orcarouter/auto
```

## Interactive Chat

### Have a Rust programming conversation

```bash
orca chat
# You: What's the best way to handle errors in Rust?
# Assistant: [response about Result and ?]
# You: Can you show me an example?
# Assistant: [code example]
# You: exit
```

### Get design advice

```bash
orca chat
# You: Should I use a trait or a struct for this?
# Assistant: [design discussion]
# You: What about performance?
# Assistant: [performance analysis]
# You: exit
```

## Advanced Scenarios

### Create a web server

```bash
orca agent "Create a basic Axum web server:
- Cargo.toml (with axum and tokio dependencies)
- src/main.rs (server setup with routes)
- src/handlers.rs (request handlers)
- src/models.rs (response models)"
```

### Database integration

```bash
orca agent "Create a database abstraction layer:
- Cargo.toml (with sqlx dependency)
- src/db.rs (database connection and queries)
- src/models.rs (data models)
- examples/query.rs (usage example)"
```

### Generate testing utilities

```bash
orca agent "Create test_utils.rs with helper functions for:
- Creating test fixtures
- Temporary file management
- Mock object creation
- Custom assertions"
```

### Create GitHub Actions workflow

```bash
orca agent "Create .github/workflows/ci.yml for a Rust project with:
- cargo test
- cargo clippy
- cargo fmt --check
- builds for Linux, macOS, and Windows"
```

## Performance-Focused Examples

### Optimize hot loop

```bash
orca agent "Read hot_loop.rs, identify performance bottlenecks, and rewrite using SIMD or other optimization techniques" -m openai/gpt-4o
```

### Memory-efficient data structure

```bash
orca agent "Create an efficient-memory.rs with a data structure that minimizes allocations while processing large datasets"
```

### Concurrent processing

```bash
orca agent "Create parallel.rs with a multi-threaded or async implementation for processing batches of items"
```

## Tips & Tricks

### Use verbose mode to debug agent iterations

```bash
orca agent "Your Rust task" -v
```

### Increase iterations for complex tasks

```bash
orca agent "Complex multi-file project setup" --max-iterations 15
```

### Chain operations

```bash
orca agent "List files in src/, then read src/main.rs, then create a refactored version"
```

### Experiment with temperature

```bash
# More consistent (default)
orca agent "Generate function" -m orcarouter/auto -t 0.5

# More creative
orca agent "Generate function" -m orcarouter/auto -t 1.5
```

## Workflow Examples

### Building a microservice

```bash
orca agent "Create a Rust microservice with:
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
orca agent "Create a comprehensive test setup:
1. Cargo.toml (with test dependencies)
2. src/lib.rs (library with public API)
3. src/main.rs (binary that uses lib)
4. tests/integration_test.rs (integration tests)
5. benches/bench.rs (performance benchmarks)"
```

### Create a library

```bash
orca agent "Scaffold a reusable Rust library:
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
orca agent "Create a feature-rich CLI tool using clap:
1. Cargo.toml (with clap v4)
2. src/main.rs (argument parsing)
3. src/commands/ (subcommand modules)
4. src/config.rs (config management)
5. src/errors.rs (error handling)"
```
