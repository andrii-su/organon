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
- `release`

Examples:

- `feat/search-api`
- `fix/indexer-empty-text`
- `chore/update-deps`
- `feat(cli): add search command`

## Local Checks

Rust:

```bash
cargo test --workspace --all-targets
```

Python:

```bash
uv sync --group dev
uv run --group dev ruff check ai
uv run --group dev pytest
```

Setup script:

```bash
bash -n setup.sh
```

## CI

GitHub Actions currently enforce:

- Rust workspace tests
- Python tests
- Python lint for `ai/`
- `setup.sh` shell syntax
- branch naming
- commit message semantics
- PR title semantics

## Releases

- Merges to `main` trigger `semantic-release`.
- Release tags use `v<version>`.
- `CHANGELOG.md` is updated automatically.
- GitHub Release notes are generated from Conventional Commits.
