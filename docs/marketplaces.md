# Marketplaces & distribution

Where Organon's MCP server can be listed, and what each channel needs.

| Channel | Status | Requires |
|---|---|---|
| Claude Code plugin | ✅ live | `.claude-plugin/` manifest in this repo (done) |
| Glama | ✅ live | `glama.json` in this repo (done) |
| GitHub Releases (prebuilt binaries) | ✅ live | Auto-dispatched after each merged PR passes CI and package smoke |
| Docker image (GHCR) | ✅ live | Auto-dispatched from the same release tag |
| MCP Registry (official) | ⏳ ready | submit `server.json` (OCI variant — the GHCR image satisfies it) |
| Smithery | ⏳ ready | submit `smithery.yaml` (container deploy via the GHCR image) |

The Docker image unblocks the last two channels: it's a published, runnable OCI
artifact, which is exactly what the MCP Registry (`oci` package) and Smithery
(container deploy) need.

______________________________________________________________________

## Live now

### Claude Code plugin

```bash
claude plugin marketplace add andrii-su/organon
claude plugin install organon@organon
```

Registers the `organon` MCP server (stdio). The `organon` binary must be on
`PATH`. See [`plugins/organon/`](../plugins/organon/).

### Glama

Glama auto-indexes public GitHub MCP repos. [`glama.json`](../glama.json) claims
ownership of the listing. Authenticate with GitHub on glama.ai to manage it — no
package publishing required.

### Prebuilt binaries

Each merged PR to `main` gets a unique SemVer patch tag after CI and package
smoke pass. The tag points to a release commit with matching Cargo and Python
versions; the commit is reachable by the tag, not added to `main`. The first
automated release uses the current `1.4.0` manifest baseline, then increments
the patch number. The Release workflow attaches binaries for macOS (arm64/x64)
and Linux (x64/arm64). Rerunning the coordinator reuses the existing tag.

### Docker image (GHCR)

The same tag also builds and pushes `ghcr.io/andrii-su/organon` (see
[`Dockerfile`](../Dockerfile) and `.github/workflows/docker.yml`). It bundles the
Rust binary + Python layer, so all MCP tools work from one image.

```bash
# Index a project (persist state in a named volume)
docker run --rm -v "$PWD:/workspace" -v organon-data:/data \
  ghcr.io/andrii-su/organon index /workspace

# Serve the MCP server over stdio
docker run -i --rm -v "$PWD:/workspace" -v organon-data:/data \
  ghcr.io/andrii-su/organon mcp --scope /workspace
```

State (graph DB, vectors, model cache) lives under `ORGANON_HOME=/data`; mount a
named volume to persist it. Mount your project at `/workspace`.

______________________________________________________________________

## Ready to submit (once the image is published)

The MCP Registry and Smithery need a published runnable artifact — the GHCR image
above satisfies both. Submit after the first `vX.Y.Z` tag has pushed the image.

### MCP Registry — `server.json`

Name: `io.github.andrii-su/organon`. OCI variant (uses the published image):

```json
{
  "$schema": "https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json",
  "name": "io.github.andrii-su/organon",
  "description": "Local-first semantic filesystem layer for AI agents — entity graph, lifecycle, semantic search over MCP.",
  "version": "1.4.0",
  "repository": { "url": "https://github.com/andrii-su/organon", "source": "github" },
  "websiteUrl": "https://github.com/andrii-su/organon",
  "packages": [
    {
      "registryType": "oci",
      "identifier": "ghcr.io/andrii-su/organon:1.4.0",
      "transport": { "type": "stdio" }
    }
  ]
}
```

Publish:

```bash
mcp-publisher init
mcp-publisher login github      # device-code auth at github.com/login/device
mcp-publisher publish
```

Or automate from CI with GitHub OIDC (`id-token: write`, no stored secrets).

> The `oci` package bundles the Rust binary + Python layer, so all 13 tools work.
> A `cargo` package (after `cargo publish` of `organon-core` → `organon-mcp` →
> `organon-cli`) is an alternative, but the cargo install would still need the
> Python layer separately for vector search.

### Smithery — `smithery.yaml`

```yaml
startCommand:
  type: stdio
  configSchema:
    type: object
    properties:
      scope:
        type: string
        description: Directory scope for the session
        default: "."
  commandFunction: |
    (config) => ({ command: "organon", args: ["mcp", "--scope", config.scope || "."] })
```

Smithery's hosted deploy expects a container — the GHCR image covers that. Point a
Smithery container deployment at `ghcr.io/andrii-su/organon` (entrypoint already
runs `organon mcp`). Lowest priority of the channels, but no longer blocked.

______________________________________________________________________

## Recommended order

1. ✅ Claude Code plugin + Glama + release + Docker workflows (done)
2. Push a `vX.Y.Z` tag → publishes binaries **and** the GHCR image
3. Claim the Glama listing on glama.ai (GitHub auth)
4. Submit `server.json` (OCI variant) to the MCP Registry via `mcp-publisher`
5. Optionally add a Smithery container deployment pointing at the GHCR image
