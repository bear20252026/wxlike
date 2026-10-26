# G16 CI job fragments

This directory is the **only** place G16 defines cloud CI. The files here are
workflow *fragments*: `compose.py` merges them into the workflow files under
`.github/workflows/`, which are owned by G00 (see
`integration-requests/G16/G16-01.yaml`). G16 never edits `.github/workflows/`
directly.

## Gate matrix (C0–C11)

| Gate | Job | Blocking | Executes when | What it proves |
|---|---|---|---|---|
| C0 | `select` | yes | always | Changed paths are classified; contract/CI changes force the full matrix |
| C1 | `fmt` | yes | rust | `cargo fmt --check` |
| C2 | `clippy` | yes | rust | `cargo clippy --workspace --all-targets -- -D warnings` |
| C3 | `test` | yes | rust | Workspace tests on Linux, Windows and macOS |
| C4 | `contract` | yes | contract or rust | JSON contracts parse, entry/action IDs are unique, seed ↔ registry mapping is complete, API fixtures are stable |
| C5 | `native` | yes | rust | `cargo build --workspace --all-targets --locked` on all three OSes |
| C6 | `wasm` | yes\* | rust | WASM target compiles (`wxlike-client-web` on wasm32-unknown-unknown). \*The wasm-bindgen entry / JS packaging in `apps/desktop-web` is still open (G01-03); it blocks **release**, not day-to-day CI |
| C7 | `ui` | yes | ui or desktop | React typecheck, production bundle, wire self-test, message-index contract |
| C7b | `web-v2` | yes | ui | `apps/desktop-web` (React + Vite, G10): `npm ci` → typecheck → lint → test → build, plus `packages/mini-sdk` contract-parity tests |
| C8 | `flutter`, `android`, `ios` | yes\* | mobile, contract or rust | UniFFI bindings regenerate, `flutter analyze`, `flutter test`, release-mode APK with symbol/signature evidence. \*`ios` records `apps/mobile/ios` absence as a tracked gap; the release job fails closed |
| C9 | `desktop`, `desktop-package` | yes | desktop or ui | Tauri command tests; Windows/macOS/Linux installers; updater signature negative test; per-platform manifest with hashes |
| C10 | `audit` | yes | rust or ci | typos, `cargo deny`, `cargo audit` (root and Tauri), both lockfiles `--locked`, action pins, workflow policy, composed-workflow freshness |
| C11 | `coverage`, `unwrap`, `image`, `e2e`, `performance` | yes | rust, contract or ci | Incremental security-path coverage; no new `unwrap()/expect()`; container image constraints and smoke; HTTP e2e (success/empty/param/unauthorized/timeout/cancel/restart) and measured latency/throughput budgets |
| — | `gate` | yes | always | Aggregates all of the above. Failed/cancelled dependencies fail; skipped dependencies pass only when C0 selected them out |

`C6` is deliberately split: the wasm job compiles every workspace crate that
declares a wasm32 target (today `wxlike-client-web`), and the remaining gap —
wiring the wasm module into the `apps/desktop-web` build through a
wasm-bindgen entry — is tracked in `policy/known-gaps.json`. The **release**
workflow calls `check_contracts.py --require-registry` and
`check_gaps.py --release`, which fails while any `blocks_release` gap is open.

## Path selection

`select_targets.py` maps a PR's changed files to targets (`rust`, `contract`,
`ui`, `mobile`, `desktop`, `ci`). The rules live in
`policy/paths.json`. Two rules matter:

- A change under `contracts/**`, `crates/types/**`, `ci/**`, `.github/**`,
  any workspace manifest/lock, or `rust-toolchain*` sets `full=true` and runs
  every target. A contract change can never ride on a partial matrix.
- Unrecognised non-documentation files also force `full=true`. New top-level
  directories are not silently skipped.
- Documentation-only changes select nothing; `gate` passes with an explicit
  "no code targets" summary.

The classification is fail-safe: if `git diff` cannot be computed, `full=true`.

## Caching and supply chain

- Rust caches are keyed by OS + `Cargo.lock` + toolchain version
  (`RUSTUP_TOOLCHAIN`), and separated per purpose (`clippy`, `test`, `native`,
  `doc`, `wasm`, `coverage`, `desktop`, `android`, `ios`, `e2e`, `performance`).
  No cross-purpose `restore-keys` exist.
- Node caches use `ui/package-lock.json` (and `apps/desktop-web/package-lock.json`
  for the `web-v2` job; `packages/mini-sdk` has no dependencies, so no npm
  cache or lockfile is claimed for it); Flutter uses the SDK version plus
  `apps/mobile/pubspec.lock`. No Gradle cache is configured yet, so no claim is
  made about Gradle reproducibility.
- Every `uses:` is pinned to a 40-hex commit in `policy/action-pins.json`;
  `check_pins.py` also verifies the SHA is one of the policy's approved SHAs for
  that repository. `docker://` images must use `@sha256:`. npm tools such as
  `@tauri-apps/cli@2.9.0` are version-pinned in `policy/action-pins.json` and in
  the command line.
- `permissions` is `contents: read` at workflow level. Only jobs that need more
  opt in at job level (packages, id-token, attestations, release contents) and
  are listed in `policy/required-checks.json`.
- No required job uses `continue-on-error: true`. Coverage, unwrap, image, e2e
  and performance are blocking.

## Required checks

Branch protection on `main` requires exactly one context: `gate`. That is the
only shape this workflow can satisfy: `gate` runs with `if: always()` and
`build/gate.py` re-checks every job listed in `policy/required-checks.json`,
including the ones C0 path selection skipped. Registering those job names as
required checks instead would leave a path-scoped PR waiting forever on checks
that never start, and a matrix context renames itself on every matrix edit.

`policy/required-checks.json` stays the source of truth for *what `gate`
demands* - not for what GitHub's branch protection is configured with.

The same file names the protected environments: `mobile-release` for
`android-release`/`ios-release` and `production-release` for `desktop-release`
and `release` in `release.yml`.

## Composition

```bash
# Render and validate everything without touching .github/
python ci/jobs/compose.py --check

# Write the composed workflows for review (G00 copies them into .github/)
python ci/jobs/compose.py --output-dir ci/jobs/generated
```

The base triggers, permissions and environment live in
`ci/jobs/workflows/*.workflow.yml`. Job bodies live in
`ci/jobs/<fragment>/job.yml`; an entry in `x-wxlike-fragments` may also name a
fragment file directly (`ci/jobs/build/web_v2.job.yml`). `x-wxlike-fragments`
declares the merge order; the key is removed from the output. Duplicate job
ids, unresolved `needs:` references and unpinned actions fail composition.

## Local verification

```bash
python -m unittest discover -s ci/jobs/tests -v
python ci/jobs/compose.py --check
python ci/jobs/build/select_targets.py --changed-file crates/core/src/lib.rs

# native e2e + restart + fault suites (build first)
cargo build --locked -p wxlike-server
WXLIKE_SERVER_BIN=target/debug/wxlike-server bash tests/e2e/ci.sh

# measured capacity; budgets are data, not prose
python tests/perf/run.py --server-bin target/debug/wxlike-server \
  --budgets ci/jobs/performance/budgets.json --evidence-dir /tmp/perf
```

The Python scripts use only the standard library. `compose.py` uses PyYAML,
which is installed in CI by the `audit` job. `--check-live` additionally
requires `.github/workflows` to be byte-identical to the composed output; G00
runs it after applying the files (the daily job only runs `--check`).
