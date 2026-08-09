# Contributing

Contributions are welcome. Keep changes focused, explain the user-facing reason
for them, and add tests for behavior changes.

## Report A Bug

Open a GitHub issue with:

- your Herdr version from `herdr --version`
- operating system and architecture
- steps to reproduce the problem
- expected and actual behavior
- relevant output from `herdr plugin log list --plugin angel-o.labels`

Please search existing issues first and remove private information from logs.

## Development Setup

Development requires Rust 1.89 or newer and Herdr 0.7.5 or newer.
See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for module boundaries and test
placement.

Build and link the working tree:

```bash
cargo build --release
herdr plugin link "$PWD"
```

`herdr plugin link` does not run manifest build commands. Rebuild locally after
changing Rust code.

## Make A Change

- Create a branch from `main`.
- Keep naming and numbering policy pure and isolated from Herdr socket and
  filesystem I/O.
- Preserve session isolation and manual-name ownership transitions.
- Preserve the stale-label check before renaming a tab.
- Keep plugin commands bounded and event handlers idempotent.
- Keep shell-hook tests isolated from the developer's environment and live
  Herdr session.
- Add or update tests alongside the module whose behavior changes.

Do not commit generated files under `target/` or manually built release
binaries. Version bumps and release workflow changes should be isolated in a
release-focused pull request.

## Rust State And Mutation

- Prefer immutable bindings, borrowed inputs, iterators, and returned values for
  policy and data transformation.
- Transfer ownership when a value is no longer needed; clone only when two
  independently owned values must survive.
- Use `&mut` when an operation intentionally changes a domain aggregate or a
  stateful infrastructure adapter. Keep that borrow as narrow as practical.
- Keep mutation at application and infrastructure boundaries. Naming and
  numbering policy should remain pure.
- Use small local mutable accumulators when they express an algorithm more
  directly than allocation-heavy immutable rebuilding.
- Do not introduce interior mutability to avoid an honest `&mut` API. Use
  atomics, locks, or cells only for genuine shared-state requirements, and make
  the synchronization contract explicit.
- Persist state at explicit transaction boundaries; do not hide filesystem or
  socket side effects inside domain-policy helpers.

## Verify

Run the same checks used by CI:

```bash
cargo fmt --check
cargo test --all-targets --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items --locked
tests/test-hooks.sh
scripts/check-version.sh
scripts/test-install.sh
```

## Open A Pull Request

Describe what changed, why it is needed, how it was tested, and any behavior or
compatibility implications. Keep unrelated changes in separate pull requests.

By contributing, you agree that your contribution is licensed under the
project's [MIT License](LICENSE).
