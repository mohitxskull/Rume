# CodeQL advanced setup for Rust (Cargo workspace) — research findings

Date: 2026-11-05
Sources:
- https://docs.github.com/en/code-security/code-scanning/creating-an-advanced-setup-for-code-scanning/codeql-code-scanning-for-compiled-languages
- https://docs.github.com/en/code-security/reference/code-scanning/codeql/build-options-for-compiled-languages
- https://docs.github.com/en/code-security/code-scanning/automatically-scanning-your-code-for-vulnerabilities-and-errors/configuring-code-scanning#configuring-codeql-analysis-in-your-workflow
- https://github.com/github/codeql-action (tags, init/analyze/autobuild action.yml)

## 1. Latest action versions

The CodeQL Action v4 is the current/latest major version (v3 is still supported but in maintenance). GitHub recommends referencing the **major version tag** (`v4`) so the workflow automatically picks up the latest patch release, including updated CodeQL CLI versions.

| Action | Recommended reference | Latest patch (as of 2026-11-04) |
|---|---|---|
| `github/codeql-action/init` | `@v4` | `v4.39.3` (commit `33d7b85`) |
| `github/codeql-action/analyze` | `@v4` | `v4.39.3` (commit `33d7b85`) |

The `v4` major tag and the immutable `v4.39.3` tag point to the same commit. Do not pin to a specific patch SHA unless you have a Dependabot process to keep it current.

## 2. Build mode for Rust

**Use `build-mode: none` for Rust.** This is the only build mode CodeQL supports for Rust (autobuild and manual are **not** available for Rust).

How it works:
- CodeQL uses `rust-analyzer` to compile/run build scripts (`build.rs`) and macro code, but does **not** invoke a full `cargo build`.
- A database is created from all Rust files present in the repository/workspace.
- A `Cargo.toml` or `rust-project.json` **must be present** (for a workspace, the root `Cargo.toml`).
- `rustup` and `cargo` must be installed. These are **pre-installed on GitHub-hosted `ubuntu-latest` runners**, so no toolchain setup step is needed.

**No `cargo build` step is required before `analyze`.** Adding one is unnecessary overhead; `build-mode: none` with no build steps is the recommended setup for a Cargo workspace.

Trade-off (per docs): `none` mode may miss some alerts if the repository generates code during the build process, or if dependency guesses are inaccurate. For most Rust codebases this is acceptable and is the default used by GitHub default setup.

## 3. Recommended job `permissions`

```yaml
permissions:
  security-events: write   # REQUIRED — upload SARIF results to Code Scanning
  contents: read           # REQUIRED for private repositories; optional for public
```

- `security-events: write` is mandatory for all advanced-setup code scanning workflows.
- `contents: read` is additionally required for private repositories.
- `packages` and `actions` are **not** required unless you download private CodeQL packs/registries or need GitHub Packages access.

## 4. Recommended `on` triggers

```yaml
on:
  push:
    branches: [main]
  pull_request:
    branches: [main]
  schedule:
    - cron: '20 14 * * 1'
```

- `push` on the default branch (and any protected branches) — results also map to open PRs.
- `pull_request` against the default branch — results appear as PR check annotations.
- `schedule` weekly (cron above = Mondays 14:20 UTC) — catches newly-discovered vulnerabilities even without developer activity.

## 5. Rust-specific considerations

- Only `build-mode: none` is supported for Rust — do **not** set `autobuild` or `manual`; they will not work for Rust.
- No `cargo build`/`cargo test` step between `init` and `analyze`.
- Root `Cargo.toml` (or `rust-project.json`) must exist — CodeQL analyzes the whole Cargo workspace from it.
- `rustup` + `cargo` required; pre-installed on GitHub-hosted runners.
- Optional but helpful: `dependency-caching: true` on the `init` step to cache/restore build dependencies and speed up rust-analyzer resolution in large workspaces.
- If analysis is incomplete, consider `manual` is not an option for Rust; instead ensure `Cargo.toml`/`rust-project.json` is present and dependencies are resolvable (network access or vendoring).

## 6. Recommended `runs-on`

`ubuntu-latest` (GitHub-hosted). Rust analysis does not require macOS (unlike Swift), so the cheapest Linux runner is appropriate. `windows-latest` and `macos-latest` are also supported but unnecessary for Rust.

---

## Recommended `.github/workflows/codeql.yml` for a Rust Cargo workspace

```yaml
name: "CodeQL"

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]
  schedule:
    - cron: '20 14 * * 1'

jobs:
  analyze:
    name: Analyze
    runs-on: ubuntu-latest
    permissions:
      security-events: write
      contents: read
    strategy:
      fail-fast: false
      matrix:
        language: [rust]
    steps:
      - name: Checkout repository
        uses: actions/checkout@v4

      - name: Initialize CodeQL
        uses: github/codeql-action/init@v4
        with:
          languages: ${{ matrix.language }}
          build-mode: none
          dependency-caching: true

      - name: Perform CodeQL Analysis
        uses: github/codeql-action/analyze@v4
        with:
          category: "/language:${{ matrix.language }}"
```

Notes on this config:
- `build-mode: none` is correct and sufficient for Rust — no cargo build step needed.
- `dependency-caching: true` is optional; omit it if you prefer zero cache writes or have no network to restore from.
- `fail-fast: false` ensures the Rust analysis still runs/reports even if another language matrix entry were added later and failed.
- `category` distinguishes the SARIF upload; omit it and GitHub generates one from the workflow/matrix automatically.