# Contributing

## Workflow

- Create branches as `<type>/<slug>`.
- Use Conventional Commits for commit messages.
- Use Conventional Commit format for PR titles too.

Allowed `type` values:

- `feat`
- `fix`
- `chore`
- `docs`
- `refactor`
- `test`
- `ci`
- `build`
- `style`
- `perf`
- `revert`

Examples:

- `feat/context-command`
- `fix/indexer-empty-text`
- `chore/update-deps`
- `refactor/agent-first-slim`

## Local Checks

Rust:

```bash
cargo test --workspace --all-targets
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Python:

```bash
uv sync --group dev
uv run --group dev ruff check ai tests
uv run --group dev pytest
```

Setup script:

```bash
bash -n setup.sh
```

## CI

GitHub Actions enforce:

- Rust build, tests, formatting, and clippy
- Python tests and linting
- `setup.sh` shell syntax
- branch naming
- commit message semantics
- PR title semantics

## Releases

Releases are manual for now. Keep version bumps and release notes in the same
PR when preparing a tagged release.
