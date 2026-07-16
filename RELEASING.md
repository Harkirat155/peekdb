# Releasing peekdb

This is a manual, deliberately un-automated checklist. Publishing (a tag push, a GitHub
Release, a crates.io publish) is effectively permanent — this file exists so none of that
happens by accident or by a CI job with too much authority.

## Hard gate — confirm all of this before any real public release

- [ ] `mcp-debug-wrapper.sh` (and any local debug/log files it produced under `/tmp`) are
      absent from the working tree and from git history (`git log --all -- mcp-debug-wrapper.sh`
      should return nothing).
- [ ] No real credentials anywhere in the tagged commit's history (`databases.toml` was
      never committed — confirm with `git log --all -- databases.toml`).
- [ ] `cargo package --list` contains no gitignored config/secret files — only
      `databases.toml.example`/`.env.example` should appear, never `databases.toml` itself.
- [ ] Config validation (rejecting literal passwords), audit logging, and all three
      backends' allowlists are present and covered by tests — they are as of the current
      `main`, but re-confirm if this checklist is run against an older commit.
- [ ] The full container-backed integration suite is green — this is the actual
      release-readiness bar, not "compiles and unit tests pass":
      ```bash
      docker compose -f docker-compose.test.yml up -d
      cargo test --test integration_postgres --test integration_mongo --test integration_redis \
        --test integration_mcp_serve -- --ignored --test-threads=1
      ```
- [ ] `cargo fmt --all -- --check` and `cargo clippy --all-targets --all-features -- -D warnings` are clean.
- [ ] `CHANGELOG.md` is regenerated and reviewed (not just appended blindly):
      ```bash
      git cliff --config cliff.toml -o CHANGELOG.md
      ```

## Cutting a release

1. Bump `version` in `Cargo.toml` (SemVer — see the versioning policy in `CONTRIBUTING.md`).
   Run `cargo build` once so `Cargo.lock` picks up the bump, then commit both.
2. Regenerate and commit `CHANGELOG.md` (see above).
3. Tag and push — **this triggers `.github/workflows/release.yml`, which builds binaries for
   Linux/macOS (x86_64 + aarch64) and opens a *draft* GitHub Release**:
   ```bash
   git tag v0.2.0
   git push origin v0.2.0
   ```
4. Review the draft release the workflow created (binaries attached, checksums present,
   generated release notes read sensibly), then publish it manually from the GitHub UI.
   The workflow deliberately does not auto-publish.

## Publishing to crates.io (separate, manual, do this only when actually ready)

Not run as part of the tag workflow above — deliberately manual.

Package metadata (`rust-version`, homepage, documentation, `exclude`) is maintained in
`Cargo.toml` so dry-run should stay clean.

```bash
cargo publish --dry-run   # validates packaging without uploading anything
cargo publish             # this step is irreversible: a version can never be un-published,
                           # only yanked (which still leaves it resolvable by pinned Cargo.lock files)
```

Before running the real `cargo publish`, double check the package name isn't already taken
on crates.io and that you're logged in as the intended account (`cargo login`).

After the first successful publish, prefer documenting `cargo install peekdb` (already in
README / site / `llms.txt`) over git-only install.

## Homebrew tap

Deliberately deferred until there's real external demand for it — not worth maintaining a
tap for zero users.
