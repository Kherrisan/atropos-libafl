# Repository Guidelines

**Nyx paths:** Do not hard-code machine-specific absolute host paths in Nyx scripts or runtime defaults. Derive paths from the script or repository location, `$HOME` or an `ATROPOS_NYX_*` setting.

## Project Structure

- `src/` contains the Rust fuzzer: HTTP inputs, mutation stages, OpenAPI seeding, coverage reporting, and LLM integration.
- `vendor/libafl_nyx/` holds the local LibAFL Nyx compatibility patch; preserve it when updating dependencies.
- `guest/php/` contains the PHP Nyx agent, bootstrap, and shared-memory extension. `guest/spring/` contains the Spring agent and instrumentation. `guest/common/` holds the dump helper both agents compile. `scripts/` builds the runtime, packages and provisions the guest, and runs the fuzzer.
- `coverage-tools/` pins the Composer tools used to remap and merge detailed PHP coverage. Build artifacts and WordPress data belong in the configured local Nyx data directory, not in Git.
- `output/` holds one directory per `scripts/run-fuzzer.sh` invocation, named `YYYYMMDD-HHMM` plus four hex characters. That directory is gitignored.

## Build, Test, and Development

- `cargo build` builds the Rust frontend for local development.
- `cargo build --profile nyx` builds the optimized Nyx binary profile.
- `cargo fmt --check` checks Rust formatting; run `cargo fmt` to apply it.
- `cargo test` runs the Rust unit tests embedded in modules under `src/`.
- `scripts/build-nyx-fuzzer.sh` builds the Nyx-enabled LibAFL/QEMU toolchain. `scripts/build-nyx-php.sh` builds PHP/PCOV and the guest runtime; follow the container setup in `README.md`.
- After preparing the guest and KVM, use `ATROPOS_NYX_ITERS=20 scripts/run-fuzzer.sh` for a short integration smoke run. The script creates `output/<YYYYMMDD-HHMM>-<4 hex>/`, tees stdout and stderr to `fuzzer.log` there, and starts the fuzzer with that directory as its working directory. Default corpus and objective paths are therefore `corpus/` and `objectives/` inside the run directory. Coverage reports and `llm/queue.json` are written in the same directory. There is no per-execution trace file. Relative `--corpus-dir`, `--objectives-dir`, `--seed-dir`, `--nyx-share`, `--nyx-workdir`, `--mutation-dict`, `--bug-trigger`, and `--openapi` values are resolved from the directory where the script was invoked, before that working-directory change. Nyx images, snapshots, and the QEMU work directory stay under the configured Nyx data directory.

## Coding Style

Use Rust 2021 conventions and let `rustfmt` set layout (four-space indentation). Use `snake_case` for modules and functions, `UpperCamelCase` for types, and `SCREAMING_SNAKE_CASE` for constants. Keep shell scripts executable and follow their existing `set -euo pipefail` and environment-variable conventions. Do not commit VM images, snapshots, credentials, corpus output, or `output/` run directories.

## Testing

Add focused unit tests alongside the relevant Rust module using `#[cfg(test)]` and descriptive `snake_case` test names. Run `cargo test` and `cargo fmt --check` for Rust changes. Guest or Nyx changes need an integration smoke run when the Ubuntu/KVM/Nyx environment is available; note any limitation in the review. No coverage threshold is configured.

## Commits and Pull Requests

Recent commits use short imperative summaries (for example, “Add queue coverage reports…”); follow that style and keep commits focused. Pull requests should explain the behavior changed, note configuration effects, link related issues, and list validation commands and results. Include logs or screenshots when they clarify runtime failures.

## Configuration and Secrets

Read `README.md` before changing provisioning or runtime setup. Keep local database credentials and WordPress configuration out of the repository; use the documented `ATROPOS_*` variables and local Nyx data paths for machine-specific settings.
