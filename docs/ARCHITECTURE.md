# Architecture

Herdr Labels uses a flat, layered Rust architecture. Modules are grouped by
responsibility without nested packages.

```text
shell hooks / Herdr events
            |
          main
            |
     config + settings
            |
          runner
            |
      reconciliation
       /     |      \
  naming  numbering  state
            |
           herdr

runner -> lock
config/settings/state/lock -> filesystem
```

## Entry And Configuration

- `main.rs` wires the executable together.
- `config.rs` interprets command arguments and Herdr-provided environment
  context into an invocation.
- `settings.rs` discovers and parses the user-facing TOML configuration.

Configuration parsing and invocation parsing are separate because they change
for different reasons. Recovery actions can also bypass malformed user settings.

## Application Layer

- `runner.rs` owns invocation timing, close settling, locking, exact-operation
  serialization, structural event coalescing, and the total per-process pass
  budget.
- `reconciliation.rs` applies one coherent session observation to ownership and
  labels. It combines naming and numbering into one desired tab label and
  re-reads the tab before mutation.

Keeping scheduling out of reconciliation prevents lock lifecycle and shell-hook
ordering from obscuring label ownership rules.

## Domain Policy

- `naming.rs` is pure process-to-label policy.
- `numbering.rs` is pure prefix parsing and rendering policy.

Neither module talks to Herdr or the filesystem.

## Infrastructure

- `herdr.rs` is the one-second read/write-timeout socket client and protocol
  model.
- `state.rs` persists per-session ownership transitions atomically.
- `lock.rs` provides OS-backed serialization and rerun signaling.
- `filesystem.rs` owns shared absolute-path validation, private-directory
  creation, and final-path symlink rejection.

Configuration and persistence modules depend on filesystem policy rather than
reimplementing path-safety rules at each caller.

## Tests

Production modules declare external `tests` submodules whose source lives under
`tests/unit/`. This keeps private-unit coverage without placing hundreds of test
lines beside production logic. Repository-level manifest and shell integration
tests remain directly under `tests/`.

Tests that start shells must isolate `HOME`, shell startup variables, Herdr
context, and the invoked binary. They must never source a real installed hook or
connect to a live Herdr socket.
