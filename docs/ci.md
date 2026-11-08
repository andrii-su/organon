# CI and release checks

Every PR to `main` runs Rust and Python tests, lint, MCP protocol and filesystem
query contracts, a migration smoke test against the latest published binary,
and Package smoke builds. The Security workflow reviews new dependencies and
analyzes Rust and Python with CodeQL. Dependabot updates Cargo, uv, and Actions
dependencies weekly.

The migration smoke creates a database with the previous release, opens it with
the current binary, and checks that its entities and history survive while new
changes remain recordable. The filesystem query contract uses a small local
fixture and checks FTS search, route candidates, context, impact, and history
through MCP without downloading an embedding model.

After a merged PR passes CI, Security, and Package smoke, the release coordinator
creates a SemVer tag. Release and Docker workflows publish from that tag. The
Release workflow verifies all four archives and checksums, downloads the Linux
archive, runs its binary, and compares its version with the tag. The Docker
workflow pulls the pushed image by digest and checks its version. Both workflows
generate and verify build attestations. A failed publication or verification is
visible as a failed workflow run.

Package smoke and Release cache Rust build inputs by runner OS, target, and
Cargo lockfile. Docker builds use the GitHub Actions BuildKit cache. Check the
Actions usage and performance metrics to compare run times after the caches
warm up.
