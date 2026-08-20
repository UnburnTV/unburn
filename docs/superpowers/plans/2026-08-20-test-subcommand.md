# Test Subcommand Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `--test-pattern PATTERN` with a standalone `unburn test [PATTERN]` utility that defaults to gray 25 and exits when the pattern closes.

**Architecture:** Clap parses and validates the optional pattern directly into `TestPattern`. The main executable routes `test` before binding the normal instance-control socket, starts the existing overlay application without the configuration GUI, and owns a transient event loop that ends as soon as the pattern is dismissed.

**Tech Stack:** Rust 2021, clap 4 derive API, existing overlay service and unit-test framework.

## Global Constraints

- Every string emitted to a terminal must contain ASCII characters only.
- Work directly on the current feature branch.
- Do not add dependencies.
- Preserve Space and arrow-key controls supplied by the overlay backend.

---

### Task 1: Parse and Validate the Test Subcommand

**Files:**
- Modify: `src/cli.rs`
- Modify: `src/overlay/mod.rs`

**Interfaces:**
- Consumes: `TestPattern::parse(&str) -> Option<TestPattern>`
- Produces: `Command::Test { pattern: TestPattern }`

- [ ] **Step 1: Write failing CLI tests**

Replace the old `--test-pattern` assertion with assertions that `unburn test` produces gray 25, `unburn test 50` produces gray 50, and `--test-pattern` is rejected. Add an assertion that an unknown pattern is rejected.

- [ ] **Step 2: Run the CLI tests and verify the expected failure**

Run: `cargo test cli::tests --lib`

Expected: FAIL because `test` is not a recognized subcommand.

- [ ] **Step 3: Add the minimal parser implementation**

Import `crate::overlay::TestPattern`, add:

```rust
Test {
    #[arg(
        default_value = "25",
        value_name = "PATTERN",
        value_parser = parse_test_pattern
    )]
    pattern: TestPattern,
},
```

Remove `Args::test_pattern` and parse values with:

```rust
fn parse_test_pattern(value: &str) -> Result<TestPattern, String> {
    TestPattern::parse(value).ok_or_else(|| format!("unknown test pattern: {value}"))
}
```

Update the parser documentation in `TestPattern::parse`.

- [ ] **Step 4: Run the CLI tests and verify they pass**

Run: `cargo test cli::tests --lib`

Expected: all CLI tests PASS.

### Task 2: Run Test Patterns as a Transient Utility

**Files:**
- Modify: `src/app.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `Command::Test { pattern: TestPattern }`, `App::test_pattern()`, `App::pump()`
- Produces: `run_test(args: &Args) -> Result<ExitCode, String>`

- [ ] **Step 1: Write a failing application-state test**

Add a unit test proving that the parsed pattern from `Command::Test` is converted to the initial `TestPatternState` by a small pure helper used by `App::start`.

- [ ] **Step 2: Run the application test and verify the expected failure**

Run: `cargo test app::tests::test_command_selects_the_initial_pattern --lib`

Expected: FAIL because the helper does not yet exist.

- [ ] **Step 3: Initialize application state from the subcommand**

Add a helper that matches `args.command` and returns `Some(TestPatternState)` only for `Command::Test`. Use it in `App::start` instead of the removed option field.

- [ ] **Step 4: Add the transient runner**

Route `Command::Test` before normal IPC socket setup. Start `App` with a wake channel, pump overlay events, and wait on the channel until `App::test_pattern()` becomes `None` or the backend disconnects. Do not call `gui::run` and do not bind `ipc::Server`.

- [ ] **Step 5: Run focused tests**

Run: `cargo test cli::tests app::tests --lib`

Expected: all focused tests PASS.

- [ ] **Step 6: Commit and publish the implementation**

```bash
git add src/cli.rs src/overlay/mod.rs src/app.rs src/main.rs docs/superpowers/plans/2026-08-20-test-subcommand.md
git commit -m "Replace test pattern flag with test subcommand"
git push -u origin cursor/test-subcommand-cc90
```

- [ ] **Step 7: Run full verification**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release
```

Expected: every command exits 0 with no warnings or test failures.
