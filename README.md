# code-sandbox

A code-execution API that runs untrusted user code inside a **wasmtime** sandbox.
Submit a snippet of Python or TypeScript/JavaScript; it runs in an isolated WASM
guest with no host filesystem, no network, a memory cap, and a wall-clock
timeout; you get back stdout/stderr/exit code.

This is **iteration 1: the local executor**. It proves the runtime and security
model end-to-end on one box. The Kubernetes "warm pod" scaling layer is designed
below and is the next iteration.

## Why wasmtime

The workload is **pure compute** (stdin/args in, stdout/stderr out — no network,
no arbitrary packages), which is exactly where WASM shines:

- **Capability-based isolation.** A guest can only touch what the host explicitly
  grants. We grant a single read-only directory (the user's code) and nothing
  else — no host FS, no sockets, no clock manipulation.
- **Deterministic resource control.** Linear-memory cap via `StoreLimits`;
  wall-clock timeout via epoch interruption; (optional) CPU metering via fuel.
- **Fast startup.** Modules are compiled once and kept warm in-process, so each
  run is just a fresh `Store` + instance — microseconds, not container seconds.

Both languages run *inside* wasm:

| Language           | How it runs                                                        |
|--------------------|-------------------------------------------------------------------|
| Python             | CPython compiled to `wasm32-wasi` (`runtimes/python.wasm`)         |
| JavaScript         | QuickJS compiled to `wasm32-wasi` (`runtimes/qjs.wasm`)            |
| TypeScript         | Type-stripped/transformed to JS host-side (swc), then run in QuickJS |

> TypeScript transpilation is a **trusted, host-side** step — it only parses and
> strips/lowers types, it never executes user code. The emitted JS is what runs
> in the sandbox.

## Quick start

```bash
# 1. One-time: fetch/build the wasm runtimes into runtimes/
./scripts/setup-runtimes.sh

# 2. Build (TypeScript support is behind a feature flag)
cargo build --release --features typescript

# 3. Run
./target/release/code-sandbox          # listens on 127.0.0.1:8080
```

Env vars: `BIND` (default `127.0.0.1:8080`), `PYTHON_WASM`, `QJS_WASM` (override
runtime paths; default to `runtimes/*.wasm`), `CONSOLE` (see below).

### Browser playground

A self-contained testing UI is available at **`/console`**, but only when the
server is started with the `CONSOLE` flag — it is **off by default** so it can't
be deployed by accident:

```bash
make console                 # http://127.0.0.1:8080/console
# or:  CONSOLE=1 ./target/release/code-sandbox
```

It lets you pick a language, edit code, set stdin/args/timeout, and run
(⌘/Ctrl+Enter) against `/run`, showing stdout/stderr/exit code/duration. Keep it
disabled in production.

## API

### `POST /run`

```jsonc
{
  "language": "python" | "typescript" | "javascript",
  "code": "print('hi')",
  "stdin": "optional input",      // piped to the program's stdin
  "args": ["optional", "argv"],   // real argv: Python sys.argv[1:], JS scriptArgs.slice(1)
  "timeout_ms": 5000              // clamped to the server max (30s)
}
```

Response:

```jsonc
{
  "stdout": "hi\n",
  "stderr": "",
  "exit_code": 0,        // null if trapped or timed out
  "duration_ms": 26,
  "timed_out": false,
  "trapped": false,      // e.g. hit the memory cap
  "error": null          // set when a run never started (e.g. TS compile error)
}
```

### `GET /languages` — which runtimes are loaded.
### `GET /health` — liveness.

### Examples

```bash
curl -sX POST localhost:8080/run -H 'content-type: application/json' -d '{
  "language":"python","code":"import sys;print(sum(int(x) for x in sys.stdin.split()))","stdin":"1 2 3 4"
}'
# {"stdout":"10\n","stderr":"","exit_code":0,...}

curl -sX POST localhost:8080/run -H 'content-type: application/json' -d '{
  "language":"typescript","code":"enum E{A,B};const x:number=E.B;console.log(x)"
}'
# {"stdout":"1\n",...}
```

## Security model (what's verified today)

| Attempt                       | Result                                             |
|-------------------------------|----------------------------------------------------|
| Read host file (`/etc/passwd`)| `FileNotFoundError` — host FS not mounted           |
| Open a socket                 | `OSError: Not supported` — no network capability    |
| Write to its own code dir     | `PermissionError` — code dir mounted read-only      |
| Infinite loop                 | Killed at the wall-clock deadline (epoch interrupt) |
| Exhaust memory                | Trapped at the `StoreLimits` memory cap             |

## Architecture

### Today (iteration 1)

```
HTTP client ──▶ axum ──▶ spawn_blocking ──▶ Sandbox::execute
                                              │
                                              ├─ pick precompiled Module (warm)
                                              ├─ fresh Store + StoreLimits (mem cap)
                                              ├─ WASI ctx: ro /sandbox, captured
                                              │  stdout/stderr, optional stdin
                                              ├─ epoch watchdog thread (timeout)
                                              └─ instantiate + call _start
```

Source layout:

- `src/api.rs` — request/response types.
- `src/runtime.rs` — the wasmtime engine: limits, isolation, timeout, execution.
- `src/transpile.rs` — TS→JS (host-side, feature-gated).
- `src/main.rs` — axum server.

### Next (iteration 2): Kubernetes warm pods

The local design already contains the key idea — **pay compile/startup cost once,
keep it warm, do only per-run work on the hot path**. Scaling that out:

```
                    ┌─────────────┐
   client ───▶ API / Dispatcher ──┼─▶ picks an idle warm pod, sends the job
                    └─────────────┘        │
                          ▲                ▼
                    Pool Manager     ┌──────────────┐   ┌──────────────┐
                    (keeps N idle    │ Executor Pod │   │ Executor Pod │  ...
                     pods per lang   │  (this bin)  │   │  (this bin)  │
                     warm; scales    │ modules warm │   │ modules warm │
                     on demand)      └──────────────┘   └──────────────┘
```

- **Warm pod = a pod already running this executor** with `python.wasm`/`qjs.wasm`
  compiled and resident. A job is just an HTTP/gRPC call to `/run` — no cold
  start. This is the pod-level analog of the in-process warm `Module` today.
- **Pool Manager** keeps a target number of idle pods per language warm (a
  Deployment + custom controller, or KEDA/HPA on a "pending jobs" metric).
  On a burst, dispatch queues while the pool scales; scale to zero when idle.
- **One job per pod at a time** for the strongest blast-radius isolation (pod is
  recycled after N runs), OR many concurrent wasm instances per pod for density —
  a tunable. wasmtime already isolates instances; the pod boundary adds defense
  in depth (seccomp, non-root, read-only rootfs, no egress NetworkPolicy).
- **Pod hardening** (belt-and-suspenders around the wasm sandbox): `runAsNonRoot`,
  `readOnlyRootFilesystem`, dropped capabilities, seccomp `RuntimeDefault`,
  restrictive `NetworkPolicy`, tight CPU/mem `resources`.
- **Dispatcher** owns queueing, per-tenant rate limits/quotas, and result
  return. Precompiled modules can be `Module::serialize`d into the image so pod
  startup skips even the Cranelift compile.

## Roadmap

- [ ] CPU fuel metering (in addition to wall-clock timeout).
- [ ] `Module::serialize` cache baked into the image for instant pod warm-up.
- [ ] gRPC executor protocol + dispatcher service.
- [ ] Pool Manager controller + Helm chart / manifests.
- [ ] Per-tenant quotas, auth, and audit logging.
- [ ] Optional stdin size / output size limits per tenant.
