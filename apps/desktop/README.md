# Organon Desktop

Thin Tauri desktop shell over the existing Organon backend.

## Architecture

- Frontend: `React + Vite` in [src](/Users/andriisuruhov/github/organon/apps/desktop/src)
- Desktop shell: `Tauri v2` in [src-tauri](/Users/andriisuruhov/github/organon/apps/desktop/src-tauri)
- Backend integration: UI talks to the current local REST API
- API bootstrap: the Tauri layer checks `/health`; if no API is running, it starts the same `organon-cli` REST server in-process via `organon_cli::api::serve`

## Covered workflows

- Search
- Graph
- History
- Impact
- Duplicates

All flows reuse current REST/core contracts. The only additive backend work for
UI was:

- `organon-cli` library surface for reuse from Tauri
- shared graph rendering helpers
- `/graph` REST endpoint returning nodes, edges, cycles, and text/DOT/Mermaid renderings

## Run

```bash
cd apps/desktop
npm install
npm run tauri dev
```

## Build

```bash
cd apps/desktop
npm run tauri build
```

Useful smoke path without bundling:

```bash
cd apps/desktop
npx tauri build --debug --no-bundle
```

## CI

GitHub Actions includes a dedicated macOS workflow in
[.github/workflows/desktop-macos.yml](/Users/andriisuruhov/github/organon/.github/workflows/desktop-macos.yml).

It runs on `push`, `pull_request`, and `workflow_dispatch`, installs desktop
dependencies, runs frontend tests, builds the macOS `.app` bundle with
`npx tauri build --bundles app --no-sign`, and uploads the bundle as an
artifact.
