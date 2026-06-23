# Marketplaces & distribution

Where Organon's MCP server can be listed, and what each channel needs.

| Channel | Status | Requires |
|---|---|---|
| Claude Code plugin | ✅ live | `.claude-plugin/` manifest in this repo (done) |
| Glama | ✅ live | `glama.json` in this repo (done) |
| GitHub Releases (prebuilt binaries) | ✅ live | `.github/workflows/release.yml`, push a `vX.Y.Z` tag |
| MCP Registry (official) | ⏳ gated | a published package — `cargo publish` or an OCI image |
| Smithery | ⏳ gated | an npm wrapper or a Docker image |

The gated channels need a published, runnable artifact — prebuilt GitHub-release
binaries alone do not satisfy them (there is no "github-release" package type).

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

Tag a release to build and attach binaries for macOS (arm64/x64) and Linux
(x64/arm64):

```bash
git tag v0.1.0
git push origin v0.1.0
```

______________________________________________________________________

## Gated on publishing

These are ready to submit **once `organon-cli` is published to crates.io** (or an
OCI image is built). Publishing to crates.io is a deliberate, hard-to-reverse step
on the project's namespace — do it intentionally.

### MCP Registry — `server.json`

Name: `io.github.andrii-su/organon`. Valid once the `cargo` package exists:

```json
{
  "$schema": "https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json",
  "name": "io.github.andrii-su/organon",
  "description": "Local-first semantic filesystem layer for AI agents — entity graph, lifecycle, semantic search over MCP.",
  "version": "0.1.0",
  "repository": { "url": "https://github.com/andrii-su/organon", "source": "github" },
  "websiteUrl": "https://github.com/andrii-su/organon",
  "packages": [
    {
      "registryType": "cargo",
      "identifier": "organon-cli",
      "version": "0.1.0",
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

> Note: the installed binary is `organon`, but the cargo `identifier` must be the
> published crate name `organon-cli`. Publishing the CLI to crates.io also requires
> publishing `organon-core` and `organon-mcp` first (path deps need real versions),
> and the binary still needs the Python layer for vector search — graph, lifecycle,
> history, and impact tools work standalone. An OCI image is the alternative that
> bundles everything.

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

Smithery's local target is Node/`package.json`-oriented and hosted deploy expects a
container, so this needs an npm wrapper or Docker image before submission. Lowest
priority of the gated channels.

______________________________________________________________________

## Recommended order

1. ✅ Claude Code plugin + Glama + release workflow (done)
2. Decide on distribution: `cargo publish` (crates.io) or an OCI image
3. After publishing → submit `server.json` to the MCP Registry
4. After an npm wrapper / Docker image → submit `smithery.yaml` to Smithery
