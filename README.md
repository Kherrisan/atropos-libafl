# Atropos LibAFL Nyx front end

This repository implements the Atropos host fuzzer with Rust and LibAFL. It uses LibAFL's `NyxExecutor` as its only execution path: each HTTP input is sent to a Nyx VM, where a PHP CLI worker receives it through a dedicated SysV shared-memory channel. Coverage comes from the guest's patched PCOV extension through Nyx's shared bitmap. PHP crashes and Atropos bug-oracle reports are saved in the solutions corpus.

The PHP 7.4 and PCOV sources are taken from the adjacent [`atropos-legacy`](https://github.com/CISPA-SysSec/atropos-legacy) checkout. The legacy checkout is used as a build input and is not modified. This frontend no longer uses the legacy FastCGI agent: it starts one PHP CLI process and pauses inside the packaged `wp-settings.php` copy just before MU plugins load. Nyx snapshots at that point. Each restored execution injects its request globals and body through shared memory, then runs MU plugins, active plugins, the theme, `init`, `wp_loaded`, and the request handler again. The request channel has a separate SysV segment from PCOV's coverage bitmap.

## Host requirements

- Ubuntu 24.04 x86_64 with KVM and a CPU that supports virtualization.
- Nix, Rust stable via rustup, and an active systemd user manager.
- A sibling `atropos-legacy` checkout with its patched PHP and PCOV sources.
- A WordPress source tree, defaulting to `../wordpress`.
- Network access while building and provisioning the guest.

The scripts use a Nixpkgs 22.11 build shell for PHP 7.4's older dependencies, MariaDB, QEMU build tools, and cloud-image utilities. PHP, PCOV, MariaDB, the fuzzer, and guest image are built or run directly on the host; Docker is not used.

Install Rust if needed:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
. "$HOME/.cargo/env"
```

If either checkout is elsewhere, set these variables for the relevant commands:

```sh
export ATROPOS_LEGACY_ROOT=/path/to/atropos-legacy
export ATROPOS_WORDPRESS_ROOT=/path/to/wordpress
```

## Build the fuzzer and guest runtime

Build LibAFL with the Nyx profile. This checks out QEMU-Nyx and Packer and builds QEMU-Nyx as a static, non-LTO binary. The local `libafl_nyx` compatibility patch skips Packer's legacy 32-bit loader initramfs, which is only needed for kernel boot mode; this frontend boots a full Ubuntu disk image instead:

```sh
scripts/build-nyx-fuzzer.sh
```

Build PHP 7.4 CLI/CGI, Nyx-aware PCOV, the Atropos shared-memory PHP extension, PHP_CodeCoverage 9.2.31, phpcov 8.2.1, and the Nim guest agent directly on the host. The build adds two PCOV runtime controls so detailed coverage collection can be enabled only for `coverage_dump` inputs. Composer installs the pinned reporting tools into the local Nyx artifact directory; the original legacy checkout stays unchanged. The PHP CLI is installed under `~/.local/opt/atropos-libafl-nyx-php`, while guest files are kept under `~/.local/share/atropos-libafl/nyx/guest`:

```sh
scripts/build-nyx-php.sh
```

The PHP build uses the legacy tree's `php-7.4-patched` and `pcov-patched` source directories. `php-cgi` is retained in the runtime for compatibility, while the Nyx guest agent runs `php-cli`. The build compiles the channel extension against the same PHP headers and no longer installs a Nim FastCGI package.
If PHP and PCOV have already built with this shared-memory runtime but the Nim guest-agent step needs retrying, run `ATROPOS_NYX_REUSE_PHP=1 scripts/build-nyx-php.sh`.

## Prepare the WordPress database

Initialize the per-user MariaDB service and WordPress tables on the host. This creates local credentials in `~/.config/atropos-libafl/wordpress-db.env` and writes a local `wp-config.php` into the WordPress tree if one is not already present:

```sh
scripts/setup-wordpress.sh
```

The service listens on `127.0.0.1:33060`. Its database is dumped into the local Nyx guest bundle by the next step; no database or credential file is added to this Git checkout.

## Enable KVM's Nyx backdoor

This Nyx mode uses the generic KVM VMware backdoor and compile-time PCOV instrumentation. It does not require a fixed KVM-Nyx kernel or Intel PT. Enable the KVM module parameter once:

```sh
sudo scripts/enable-kvm-nyx.sh
```

The script writes `/etc/modprobe.d/atropos-nyx.conf`, reloads the KVM modules with `enable_vmware_backdoor=Y`, and adds the current user to the `kvm` group if needed. If it adds the group membership, log out and back in before continuing. Reloading KVM will interrupt VMs using the module, so stop those first.

## Create the Nyx guest and snapshot

Package the local WordPress tree and database, then create a checksummed Ubuntu 24.04 cloud-image guest and install MariaDB and the Atropos runtime:

```sh
scripts/package-nyx-guest.sh
scripts/create-nyx-vm.sh
```

`create-nyx-vm.sh` uses standard QEMU under TCG, `cloud-init`, and a temporary user-mode network connection to provision the guest so disk writes persist. The Nyx-specific QEMU binary is used for KVM pre-snapshot creation and fuzzing. The provisioning boot does not require KVM. On the first KVM-Nyx boot, guest services initialize MariaDB, import the WordPress database, start the PHP CLI worker, and wait until it reaches the pre-plugin checkpoint. The agent takes its Nyx snapshot there, after WordPress has loaded its core/database state but before MU plugins, active plugins, or the theme run. The pre-snapshot service detects the Nyx CPU, disables itself in the snapshot state, and issues `HYPERCALL_KAFL_LOCK`. The script writes LibAFL's `config.ron` and `default_config.ron` after the pre-snapshot is available.

The checkpoint is for single-site WordPress with no early-loading content drop-ins. Packaging stops if `wp-config.php` reads request superglobals, multisite is enabled, or `advanced-cache.php`, `db.php`, `object-cache.php`, `maintenance.php`, or `sunrise.php` is present, since those can consume request values before the checkpoint.

Large images, snapshots, and the local database bundle stay under `~/.local/share/atropos-libafl/nyx` by default. To use another location, set `ATROPOS_NYX_DATA_DIR`; `ATROPOS_NYX_SHARE`, `ATROPOS_NYX_WORKDIR`, `ATROPOS_NYX_VM_DIR`, `ATROPOS_NYX_VM_IMAGE`, `ATROPOS_NYX_PRESNAPSHOT`, and `ATROPOS_NYX_QEMU` can override individual paths. These local files contain the WordPress database and its credentials and are created with user-only permissions.

If provisioning is interrupted, rerun `scripts/create-nyx-vm.sh`. It resumes from the local image. When the packaged guest bundle changes, it reruns cloud-init on the existing disk and moves the old pre-snapshot aside before creating a fresh one; previous pre-snapshot files are retained with a `.before-bundle-*` suffix.

## Run

```sh
ATROPOS_NYX_ITERS=1000 scripts/run-fuzzer.sh
```

The Rust binary is `target/nyx/atropos-libafl`. At startup it checks for `config.ron`, loads the VM image and pre-snapshot through LibAFL Nyx, adds OpenAPI-derived seeds (or the built-in WordPress batch seed), then fuzzes through `NyxExecutor`.

Useful environment variables:

| Variable | Default | Purpose |
| --- | --- | --- |
| `ATROPOS_NYX_SHARE` | `~/.local/share/atropos-libafl/nyx/share` | Nyx config and snapshot references |
| `ATROPOS_NYX_WORKDIR` | `~/.local/share/atropos-libafl/nyx/workdir` | QEMU-Nyx work and guest dumps |
| `ATROPOS_NYX_CPU` | `0` | Nyx worker ID |
| `ATROPOS_NYX_TIMEOUT_SECS` | `2` | Per-input execution timeout |
| `ATROPOS_NYX_COVERAGE_TIMEOUT_SECS` | `60` | Per-testcase timeout while collecting detailed queue coverage |
| `ATROPOS_NYX_ITERS` | unlimited | Stop after this many stage passes (each may run multiple mutation candidates) |
| `ATROPOS_OUTPUT_DIR` | `<WordPress directory>/atropos-output` | Corpus, solutions, and coverage output directory |
| `ATROPOS_OPENAPI` | unset | OpenAPI YAML used to seed requests |
| `ATROPOS_MUTATION_DICT` | unset | Optional AFL++-format dictionary for structured key and string-value mutations |
| `ATROPOS_LLM_REQUESTS_PER_SESSION` | `1` | Independent requests to ask for and evaluate in one ACP session (1–32) |

The corpus and saved crashes/oracle hits are written to `nyx-corpus/` and `nyx-solutions/` under `ATROPOS_OUTPUT_DIR`. The optional LLM stage retains the existing `ATROPOS_LLM_*` configuration. When its stall/probability gate fires, it replays every enabled corpus testcase with `coverage_dump` enabled and RedQueen disabled, saves each fresh guest dump temporarily, remaps guest source paths with PHP_CodeCoverage, and uses host-side phpcov to rebuild `coverage/queue.cobertura.xml` and `coverage/queue.cov` from that pass only. The last successfully published queue report stays available until a later scan has a replacement; a failed or interrupted scan cannot erase it. The per-testcase request summaries and any failed corpus IDs are written to `llm/queue.json`; temporary per-testcase coverage files are removed after merging. The agent reads the queue-level Cobertura report and request manifest to generate one or more independent requests in a single ACP session, then Atropos evaluates each request separately; requests do not inherit the currently scheduled testcase. Rust transports the reports and invokes PHP_CodeCoverage and phpcov; it does not parse coverage rows or generate Cobertura XML. Ordinary fuzzing coverage is still read directly from Nyx's bitmap observer. The guest PHP memory limit is 512 MiB for detailed coverage collection; the host PHP CLI uses a 2 GiB limit while remapping and merging. Detailed guest coverage is started in the pre-plugin checkpoint and finalized from PHP's shutdown handler, so it also runs when the REST endpoint calls `die()`.

Set `ATROPOS_OPENAPI=/path/to/openapi.yaml` to create seeds from an OpenAPI document. Without it, the fuzzer uses the built-in WordPress batch seed. The LLM stage runs after 50 executions without a new corpus input, with an 80% chance per eligible execution. On each trigger, one ACP session can return multiple independent `HttpInput` requests; Atropos evaluates them sequentially. Set `ATROPOS_LLM_REQUESTS_PER_SESSION` to request 1–32 inputs (default `1`). It uses Node.js/npm through `npx`; the default `codex` provider expects `~/.codex/auth.json` and launches Codex ACP with `INITIAL_AGENT_MODE=agent-full-access`, while `ATROPOS_LLM_PROVIDER=claude` uses `~/.claude`. Configure `ATROPOS_LLM_AUTH_FILE`, `ATROPOS_LLM_API_KEY`, `ATROPOS_LLM_BASE_URL`, `ATROPOS_LLM_MODEL`, `ATROPOS_LLM_STALL`, `ATROPOS_LLM_PROB`, or `ATROPOS_LLM_TIMEOUT` to override those defaults. Set `ATROPOS_SCHEMA_VIOLATION_RATE` to change the default 10% chance of generating a deliberately malformed body.

The `DeterministicStage` processes the currently scheduled testcase by trying each applicable structured mutation once, in a fixed order. Every candidate starts from the original testcase, and each changed, size-valid candidate is evaluated once; one candidate does not accumulate changes from another. The operations cover JSON paths, strings, numbers, booleans, nulls, objects, arrays, and HTTP query/header/cookie key-value pairs, plus body-subtree crossover from another enabled testcase. Candidate locations and values within an operation can still be selected randomly. A malformed-body candidate is appended with the configured `ATROPOS_SCHEMA_VIOLATION_RATE` probability. When `ATROPOS_OPENAPI` is set, property names from request-body object schemas, including nested objects and array items, join the candidates for JSON object-key insertion, renaming, and self-nesting. Set `ATROPOS_MUTATION_DICT` to an AFL++ dictionary file (`name="value"` entries) to add its UTF-8 tokens to JSON-key candidates and use them for string values and HTTP metadata keys. Without OpenAPI property names or dictionary tokens, JSON keys fall back to generated `k0`–`k99` names. The fuzzer reports an error at startup if the configured dictionary file is unreadable or malformed. Mutations that would exceed Nyx's 1 MiB input buffer are skipped. Since one stage pass can now execute several candidates, `ATROPOS_NYX_ITERS` limits stage passes, not individual target executions.

For a short smoke run after building the guest:

```sh
ATROPOS_NYX_ITERS=20 ATROPOS_OUTPUT_DIR="$PWD" scripts/run-fuzzer.sh
```

## Troubleshooting

- **Missing `config.ron`, disk image, or snapshot:** run the corresponding build/package step above. `scripts/prepare-nyx-share.sh` validates the VM artifacts and writes the LibAFL Nyx configuration.
- **`enable_vmware_backdoor` is `N`:** run `sudo scripts/enable-kvm-nyx.sh`; verify `/sys/module/kvm/parameters/enable_vmware_backdoor` prints `Y` and the current user can access `/dev/kvm`.
- **Nyx QEMU cannot start:** check `/dev/kvm`, the KVM module parameter, and the paths in `~/.local/share/atropos-libafl/nyx/share/default_config.ron`.
- **Guest provision timeout:** inspect `~/.local/share/atropos-libafl/nyx/vm/preimage-serial.log` for snapshot boot failures and the QEMU serial output from cloud-init provisioning.
- **Change the WordPress source or local database:** rerun `scripts/package-nyx-guest.sh`, then rebuild the local VM disk and its pre-snapshot as described above.
