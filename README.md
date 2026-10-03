# Atropos LibAFL Nyx front end

This repository implements the Atropos host fuzzer with Rust and LibAFL. It uses LibAFL's `NyxExecutor` as its only execution path: each HTTP input is sent to a Nyx VM, where a PHP CLI worker receives it through a dedicated SysV shared-memory channel. Coverage comes from the guest's patched PCOV extension through Nyx's shared bitmap. PHP crashes and Atropos bug-oracle reports are saved in the solutions corpus.

The PHP 7.4 and PCOV sources are taken from the adjacent [`atropos-legacy`](https://github.com/CISPA-SysSec/atropos-legacy) checkout. PCOV's bitmap path records an edge at the first counted opcode of each basic block; the rest of that tree is a build input. This frontend no longer uses the legacy FastCGI agent: it starts one PHP CLI process, compiles the site into OPcache, flushes pretty permalinks in a short-lived child process, and then pauses before `index.php` loads WordPress. Nyx snapshots at that idle point. Each restored execution injects its request globals and body through shared memory, then runs WordPress from the start so `wp-settings.php` sees the real request. The request channel has a separate SysV segment from PCOV's coverage bitmap.

## Execution environment

The `atropos-libafl-nyx` container needs access to KVM for Nyx. Keep the sibling `atropos-legacy` checkout and WordPress source tree visible in the build environment; their defaults are `../atropos-legacy` and `../wordpress`. Set `ATROPOS_LEGACY_ROOT` or `ATROPOS_WORDPRESS_ROOT` when they are elsewhere. Network access is needed while fetching pinned build dependencies and provisioning the guest.

The build environment uses a Nixpkgs 22.11 build shell for PHP 7.4 dependencies, MariaDB, QEMU build tools, and cloud-image utilities.

## Build the fuzzer and guest runtime

Build LibAFL with the Nyx profile. This checks out QEMU-Nyx and Packer and builds QEMU-Nyx as a static, non-LTO binary. The local `libafl_nyx` compatibility patch skips Packer's legacy 32-bit loader initramfs, which is only needed for kernel boot mode; this frontend boots a full Ubuntu disk image instead:

```sh
scripts/build-nyx-fuzzer.sh
```

Build PHP 7.4 CLI/CGI, Nyx-aware PCOV, the Atropos shared-memory PHP extension, PHP_CodeCoverage 9.2.31, phpcov 8.2.1, and the Nim guest agent. The build adds two PCOV runtime controls so detailed coverage collection can be enabled only for `coverage_dump` inputs. Composer installs the pinned reporting tools into the local Nyx artifact directory; the original legacy checkout stays unchanged. The PHP CLI is installed under `~/.local/opt/atropos-libafl-nyx-php`, while guest files are kept under `$ATROPOS_NYX_DATA_DIR/guest` (defaulting to `~/.nyx/guest`):

```sh
scripts/build-nyx-php.sh
```

The PHP build uses the legacy tree's `php-7.4-patched` and `pcov-patched` source directories. `php-cgi` is retained in the runtime for compatibility, while the Nyx guest agent runs `php-cli`. The build compiles the channel extension against the same PHP headers and no longer installs a Nim FastCGI package.
If PHP and PCOV have already built with this shared-memory runtime but the Nim guest-agent step needs retrying, run `ATROPOS_NYX_REUSE_PHP=1 scripts/build-nyx-php.sh`.

## Prepare the WordPress database

Initialize MariaDB and the WordPress tables. This creates local credentials in `~/.config/atropos-libafl/wordpress-db.env` and writes a local `wp-config.php` into the WordPress tree if one is not already present:

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

`create-nyx-vm.sh` uses standard QEMU under TCG, `cloud-init`, and a temporary user-mode network connection to provision the guest so disk writes persist. The Nyx-specific QEMU binary is used for KVM pre-snapshot creation and fuzzing. The provisioning boot does not require KVM. On the first KVM-Nyx boot, guest services initialize MariaDB, import the WordPress database, and start the PHP CLI worker. The worker warms OPcache, flushes permalinks through a child process, and waits before loading WordPress. The agent takes its Nyx snapshot at that idle point. The pre-snapshot service detects the Nyx CPU, disables itself in the snapshot state, and issues `HYPERCALL_KAFL_LOCK`. The script writes LibAFL's `config.ron` and `default_config.ron` after the pre-snapshot is available.

The checkpoint is for single-site WordPress with no early-loading content drop-ins. Packaging stops if `wp-config.php` reads request superglobals, multisite is enabled, or `advanced-cache.php`, `db.php`, `object-cache.php`, `maintenance.php`, or `sunrise.php` is present, since those can consume request values before the checkpoint.

Large images, snapshots, guest artifacts, and the local database bundle stay under `$ATROPOS_NYX_DATA_DIR` by default. This defaults to `~/.nyx`. To use another location, set `ATROPOS_NYX_DATA_DIR`; `ATROPOS_NYX_SHARE`, `ATROPOS_NYX_WORKDIR`, `ATROPOS_NYX_VM_DIR`, `ATROPOS_NYX_VM_IMAGE`, `ATROPOS_NYX_PRESNAPSHOT`, `ATROPOS_NYX_QEMU`, and `ATROPOS_NYX_GUEST_ARTIFACTS` can override individual paths for `scripts/prepare-nyx-share.sh` and `scripts/create-nyx-vm.sh`. These local files contain the WordPress database and its credentials and are created with user-only permissions.

If provisioning is interrupted, rerun `scripts/create-nyx-vm.sh`. It resumes from the local image. When the packaged guest bundle changes, it reruns cloud-init on the existing disk and moves the old pre-snapshot aside before creating a fresh one; previous pre-snapshot files are retained with a `.before-bundle-*` suffix.

## Run

```sh
ATROPOS_NYX_ITERS=1000 scripts/run-fuzzer.sh
```

The Rust binary is `target/nyx/atropos-libafl`. At startup it checks for `config.ron`, loads the VM image and pre-snapshot through LibAFL Nyx, and, when the corpus directory is empty, loads one seed per JSON file from `--seed-dir`. It then fuzzes through `NyxExecutor`.

Fuzzer flags, parsed by the binary:

| Flag | Default | Purpose |
| --- | --- | --- |
| `--nyx-share` | `$ATROPOS_NYX_DATA_DIR/phase-run/share-oracle` | Nyx config and snapshot references |
| `--nyx-workdir` | `$ATROPOS_NYX_DATA_DIR/workdir` | QEMU-Nyx work and guest dumps |
| `--timeout-secs` | `2` | Per-input execution timeout, from 0 to 255 seconds |
| `--seed-dir` | unset | Directory of seed JSON files, one seed per file. Required when the corpus directory is empty |
| `--corpus-dir` | `./corpus` | Fuzzing corpus directory |
| `--objectives-dir` | `./objectives` | Objective hits directory |
| `--mutation-dict` | unset | AFL++ dictionary. Repeat the flag or separate paths with commas |
| `--bug-trigger` | unset | AFL++ dictionary of bug-trigger strings inserted into or replacing JSON string values. Repeat the flag or separate paths with commas |

Useful environment variables:

| Variable | Default | Purpose |
| --- | --- | --- |
| `ATROPOS_NYX_DATA_DIR` | `~/.nyx` | Root directory for Nyx VM, snapshot, bundle, share, workdir, and guest artifacts |
| `ATROPOS_NYX_CPU` | `0` | Nyx worker ID |
| `ATROPOS_NYX_COVERAGE_TIMEOUT_SECS` | `60` | Per-testcase timeout while collecting detailed queue coverage |
| `ATROPOS_NYX_ITERS` | unlimited | Stop after this many stage passes (each may run multiple mutation candidates) |
| `ATROPOS_LLM_REQUESTS_PER_SESSION` | `1` | Independent requests to ask for and evaluate in one ACP session (1–32) |

The corpus is written to `--corpus-dir` and saved crashes/oracle hits to `--objectives-dir`. Coverage reports and `llm/queue.json` are written under the current working directory. The optional LLM stage retains the existing `ATROPOS_LLM_*` configuration. When its stall/probability gate fires, it replays every enabled corpus testcase with `coverage_dump` enabled and RedQueen disabled, saves each fresh guest dump temporarily, remaps guest source paths with PHP_CodeCoverage, and uses host-side phpcov to rebuild `coverage/queue.cobertura.xml` and `coverage/queue.cov` from that pass only. The last successfully published queue report stays available until a later scan has a replacement; a failed or interrupted scan cannot erase it. The per-testcase request summaries and any failed corpus IDs are written to `llm/queue.json`; temporary per-testcase coverage files are removed after merging. The agent reads the queue-level Cobertura report and request manifest to generate one or more independent requests in a single ACP session, then Atropos evaluates each request separately; requests do not inherit the currently scheduled testcase. Rust transports the reports and invokes PHP_CodeCoverage and phpcov; it does not parse coverage rows or generate Cobertura XML. Ordinary fuzzing coverage is still read directly from Nyx's bitmap observer. The guest PHP memory limit is 512 MiB for detailed coverage collection; the host PHP CLI uses a 2 GiB limit while remapping and merging. Detailed guest coverage is started in the pre-plugin checkpoint and finalized from PHP's shutdown handler, so it also runs when the REST endpoint calls `die()`.

Pass `--seed-dir` as a directory of JSON files when the corpus directory is empty. Each `*.json` file is one seed. A file is used when it is an object with `method`, `path`, and `body`. `method` is an HTTP method. `path` starts with `/` and contains no query string. `query`, `headers`, and `cookies` may be string maps or arrays of `[key, value]` pairs. `pin_route` defaults to true; `exec_limit`, `redqueen`, and `coverage_dump` default to off. Files that do not match this format are skipped. Startup fails when the directory is missing or contains no valid seed. Object keys and scalar values from every valid seed are added to the mutation dictionary, alongside any `--mutation-dict` tokens, and are used for JSON keys, string values, and HTTP metadata names. When the corpus directory already has inputs, pass `--seed-dir` again so those tokens are still loaded. Pass `--openapi` with one or more OpenAPI 2.0, 3.0, 3.1, or 3.2 YAML or JSON files, repeating the flag or separating paths with commas, to load operations for `MutationStage` and to add request-body property names to the mutation key candidates. A file that cannot be read or is not one of those OpenAPI versions stops startup. Parsed files with no operations leave schema-aware mutation inactive. Pass `--bug-trigger` with one or more AFL++ dictionary files to supply bug-trigger strings. When that list is non-empty, a string mutation can insert or wholly replace a value with one of those strings. The optional LLM stage is disabled by default. Set `ATROPOS_LLM_PROB` to a value greater than `0` to enable it; after 50 executions without a new corpus input, that value is the chance of an LLM request per eligible execution. On each trigger, one ACP session can return multiple independent `HttpInput` requests; Atropos evaluates them sequentially. Set `ATROPOS_LLM_REQUESTS_PER_SESSION` to request 1–32 inputs (default `1`). It uses Node.js/npm through `npx`; the default `codex` provider expects `~/.codex/auth.json` and launches Codex ACP with `INITIAL_AGENT_MODE=agent-full-access`, while `ATROPOS_LLM_PROVIDER=claude` uses `~/.claude`. Configure `ATROPOS_LLM_AUTH_FILE`, `ATROPOS_LLM_API_KEY`, `ATROPOS_LLM_BASE_URL`, `ATROPOS_LLM_MODEL`, `ATROPOS_LLM_STALL`, or `ATROPOS_LLM_TIMEOUT` to override those defaults. Set `ATROPOS_SCHEMA_VIOLATION_RATE` to change the default 10% chance of generating a deliberately malformed body.

`MutationStage` processes the currently scheduled testcase. It counts mutable sites — JSON string, number, boolean, and null leaves, objects, arrays, plus query, headers, and cookies, and the path when `pin_route` is false — then sets the candidate budget to twice that count, clamped to 8–64. Each candidate starts from the original testcase, mutates one randomly chosen site, and is evaluated on its own. When the input matches an OpenAPI operation and the chosen site has a schema, half of those mutations follow the schema: enum values, value types, `required`, and numeric or length bounds. The other half uses the unconstrained mutation. A match without a schema for that site stays unconstrained. A malformed-body candidate is still appended with the configured `ATROPOS_SCHEMA_VIOLATION_RATE` probability and does not consume budget. Property names from request-body object schemas, including nested objects and array items, join the candidates for JSON object-key insertion, renaming, and self-nesting when the mutation is not following the schema. Pass `--mutation-dict` with one or more AFL++ dictionary files (`name="value"` entries), repeating the flag or separating paths with commas, to add their UTF-8 tokens to JSON-key candidates and use them for string values and HTTP metadata keys. Repeated tokens are kept once, in first-seen order. Without OpenAPI property names or dictionary tokens, JSON keys fall back to generated `k0`–`k99` names. The fuzzer reports an error at startup if any configured dictionary or bug-trigger file is unreadable or malformed. String havoc can append a delimiter plus several printable bytes, replace a delimiter-separated token or a random span, or delete through the end or a contiguous span. Mutations that would exceed Nyx's 1 MiB input buffer are skipped. Since one stage pass can execute several candidates, `ATROPOS_NYX_ITERS` limits stage passes, not individual target executions.

For a short smoke run after building the guest:

```sh
ATROPOS_NYX_ITERS=20 scripts/run-fuzzer.sh
```

## Troubleshooting

- **Missing `config.ron`, disk image, or snapshot:** run the corresponding build/package step above. `scripts/prepare-nyx-share.sh` validates the VM artifacts and writes the LibAFL Nyx configuration.
- **`enable_vmware_backdoor` is `N`:** run `sudo scripts/enable-kvm-nyx.sh`; verify `/sys/module/kvm/parameters/enable_vmware_backdoor` prints `Y` and the current user can access `/dev/kvm`.
- **Nyx QEMU cannot start:** check `/dev/kvm`, the KVM module parameter, and the paths in the `--nyx-share` directory's `default_config.ron`.
- **Guest provision timeout:** inspect `$ATROPOS_NYX_DATA_DIR/vm/preimage-serial.log` for snapshot boot failures and the QEMU serial output from cloud-init provisioning.
- **Change the WordPress source or local database:** rerun `scripts/package-nyx-guest.sh`, then rebuild the local VM disk and its pre-snapshot as described above.
