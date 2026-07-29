# Backlog — code-sandbox

**Target tier: Agent code-exec.** Adversary = prompt-injected model output at high volume.
Cross-tenant leak is fatal; DoS is unacceptable; reproducible replay matters.
This tier makes isolation, quotas, egress control, determinism, and session semantics
**requirements**, not nice-to-haves. Source: security review in `notes.txt`.

**Legend**
- 🔴 critical · 🟠 high · 🟡 medium — severity from the review
- `[FR]` functional requirement (product behavior the tool must have); untagged = security / hardening / infra
- Effort: S ≈ <1d · M ≈ 1–3d · L ≈ >3d

---

## Defense-in-depth — the layers we protect

Untrusted code sits at the top. To cause harm it must defeat **every** layer
below it. Each band names the layer, what stops the guest, and its kind.

```
════════════════════════════════════════════════════════════════
  UNTRUSTED GUEST CODE   (adversary: prompt-injected model output)
  to cause harm it must defeat every layer below            ▼▼▼
════════════════════════════════════════════════════════════════
 1 │ CAPABILITY BOUNDARY  (wasm Linker)             [structural]
   │ socket / fork / exec / FFI were never added to the linker,
   │ so there is nothing to call. Cannot be bypassed — strongest.
────────────────────────────────────────────────────────────────
 2 │ WASI SURFACE  — the ~45 syscalls we DID grant     [trusted]
   │ fd_*, path_*, clock, random. Only as safe as wasmtime's
   │ implementation of them (3 live advisories live here).
────────────────────────────────────────────────────────────────
 3 │ MEMORY SANDBOX  — Cranelift-generated code    [trusted ★]
   │ bounds checks + control-flow integrity keep the guest inside
   │ its linear memory. THE risky layer: a compiler bug = escape.
   │ Multi-run pods ⇒ such an escape is cross-tenant.
────────────────────────────────────────────────────────────────
 4 │ RESOURCE GOVERNOR                                    [limit]
   │ 256MB/store · epoch+SIGKILL timeout · output cap ·
   │ concurrency semaphore → HTTP 429. Stops DoS & exhaustion.
────────────────────────────────────────────────────────────────
 5 │ WORKER PROCESS  — one run, then recycled       [containment]
   │ non-root · no_new_privs · seccomp · RLIMIT · private tmpfs ·
   │ per-run /packages · SIGKILL. Boxes an escape from layer 3
   │ into a throwaway process, off the sibling runs.
────────────────────────────────────────────────────────────────
 6 │ POD / CONTAINER  (runc)                        [containment]
   │ namespaces · cgroups · seccomp RuntimeDefault · readonly
   │ rootfs · drop ALL caps. Escape is boxed in the pod, not node.
────────────────────────────────────────────────────────────────
 7 │ NETWORK POLICY  — deny-all egress              [containment]
   │ even a full escape cannot phone home or exfiltrate data.
────────────────────────────────────────────────────────────────
 8 │ CLUSTER / NODE                                 [containment]
   │ RBAC · automountServiceAccountToken:false → no credentials
   │ to steal, no lateral movement across the cluster.
════════════════════════════════════════════════════════════════
  PROTECTED   host memory · other tenants' code & data ·
              the node · the cluster · outbound network
════════════════════════════════════════════════════════════════
```

**How to read it**
- `[structural]` = prevents by construction (nothing to bypass). `[trusted]` = must be *correct*; a bug here fails open. `[limit]` = caps resources. `[containment]` = doesn't stop an escape, shrinks its blast radius.
- The only two layers standing between the guest and host memory are **1** and **3**. Layer 1 can't fail; **layer 3 can** (a Cranelift bug) — that is the whole memory-safety risk, and multi-run pods make it cross-tenant.
- Layers **5–8 don't prevent** a layer-3 escape — they make it *survivable* (contained, credential-less, network-less, killed on a timer).
- The **mem-safe-interpreter spike** targets the ★: Boa/RustPython have no linear memory to smash, so they remove layer 3's risk instead of merely containing it.
- Rough status: layers 1–4 exist today (with the P0 bugs); **5** is the required in-pod process-per-run work; **6–8** are the pod-hardening work.

---

## P0 — Exploitable today, block on these

- [ ] 🔴 **Upgrade wasmtime 27.0.0 → 36 LTS (or 47.x)** · M — retires ~18 advisories incl. sandbox-escape `RUSTSEC-2026-0096` (aarch64/macOS) and `-0149` (read-only bypass). `wasmtime-wasi` preview1/p2 reorg is not a drop-in.
- [ ] 🔴 **Fix engine-global epoch watchdog** · M — one request timing out currently traps *every* in-flight run (`while True: pass` = DoS for all tenants). Replace per-request threads with one fixed-cadence ticker + per-store deadlines.
- [ ] 🔴 **Fix host-blocking timeout bypass** · L — `time.sleep(3600)` ignores `timeout_ms`; ~512 such calls fully DoS the service. Move to `async_support(true)` + async WASI + `tokio::time::timeout`, and/or hard-SIGKILL the worker at deadline+grace.
- [ ] 🔴 **Add the sleep + infinite-loop timeout tests** · S — regression guard for the two bugs above; `time.sleep` with `timeout_ms: 500` must return `timed_out`.

## P1 — Required to actually be "agent code-exec" tier

- [ ] 🟠 **Isolation model: multi-run pods** · S `[FR]` — decided: pods run many concurrent programs (a small script can't justify a pod each). Consequence: the pod contains node/cluster blast radius, but siblings share one process, so **in-pod cross-tenant isolation is required** and the memory-boundary escape (Cranelift bug → read sibling memory) is a live cross-tenant risk, not a contained one.
- [ ] 🟠 **Pod hardening** · M — `automountServiceAccountToken: false` (done ✓) + `runAsNonRoot`, `readOnlyRootFilesystem`, `drop: [ALL]` caps, `seccompProfile: RuntimeDefault`, `NetworkPolicy` deny-all egress, resource `limits`/`requests`. Contains an escape to the pod and kills credential theft.
- [ ] 🔴 **In-pod process-per-run** · L — **required** (multi-run). Pre-forked single-tenant workers: parent holds the socket, hands one job to an idle worker over a pipe, worker runs exactly one program then is recycled. Each worker non-root, `no_new_privs`, seccomp, dedicated tmpfs, `RLIMIT_AS`/`NPROC`, SIGKILL at deadline+grace. This is what makes a Cranelift/WASI escape *not* cross-tenant inside a shared pod; also fixes the host-blocking sleep-DoS for free. Highest-leverage isolation work.
- [ ] 🟠 **Global resource ceiling** · S `[FR]` — 256 MiB is per-store; add `tokio::sync::Semaphore`, return `429` when saturated, cap `max_blocking_threads`. Defines the concurrency contract.
- [ ] 🟠 **Per-run private `/packages` view** · M — shared mount + `RUSTSEC-2026-0149` = cross-tenant persistence primitive (poison a `.py`, run in every future job). Give each run its own read-only view.
- [ ] **Egress policy** · M `[FR]` — no network is granted in-guest today; enforce defense-in-depth at the pod with a deny-all `NetworkPolicy` (part of Pod hardening) and state it as a product guarantee for prompt-injected code.
- [ ] **Determinism mode** · M `[FR]` — coarsened/deterministic clock + seeded RNG so same input → same output. Needed for agent replay, grading, CI. wasmtime ships a `Deterministic` RNG.
- [ ] **Session semantics** · M `[FR]` — decide + document: each call is a fresh process, nothing persists. Make it explicit in the API contract.
- [ ] **Audit log** · S `[FR]` — per-run record (code hash, verdict, duration, limits hit) for a high-volume untrusted caller.

## P1 — Runtime direction (SPIKE — decide by measurement)

- [ ] **SPIKE: memory-safe interpreters vs CPython super-binary** · L — resolve the core fork. Deliver a short doc + numbers, don't pre-commit.
  - Path A — **fully mem-safe**: Boa-in-wasm (JS) + RustPython/Starlark (Python). Kills the Cranelift-is-my-only-memory-boundary problem; costs stdlib depth.
  - Path B — **CPython super-binary**: bundle the libs you care about into one trusted wasm build; keeps battle-tested stdlib, but memory boundary stays "Cranelift must be correct."
  - Note: pods are multi-run, so a Cranelift escape reaches sibling runs and neither the pod nor process-per-run is a *memory* boundary (process-per-run contains it *after* the escape; it doesn't prevent reading a co-scheduled worker before recycle). Path A (mem-safe interpreter, no linear memory to smash) is the only way to structurally remove that risk class — weigh heavily given multi-run.
  - **Acceptance criteria (the deciders):**
    - `[FR]` Can `polars` be compiled to wasm32-wasi and produce a working DataFrame? Compiling-to-wasm is now the **only** path — there is no VM fallback. If it can't, polars is unsupported. (Polars = compiled Rust, no official wasm wheel; treat as the primary risk of this spike.)
    - `[FR]` Does `import plotly` render a chart headless? (plotly.py leans on numpy/pandas; confirm what actually works, and whether numpy itself compiles to wasm here.)
    - Which path keeps `sqlite3` + `statistics` working? (RustPython loses CPython's C stdlib.)
    - TCB size, per-run startup ms, memory/instance.
  - Output: recommendation + a `make capabilities` run against each candidate + the definitive supported-package list.

## P2 — Correctness, verification, capability gaps

- [ ] 🟠 **Turn the 8 README security-table rows into `#[test]`s** · M — "verified" is currently a human clicking buttons; these are the highest-value integration tests and make upgrades safe.
- [ ] **Land the `make capabilities` / `capabilities-check` probe** · S — script already drafted in `notes.txt`; wire baseline + CI regression check (incl. isolation-breach detection).
- [ ] 🟡 **Fix `py-deps` native-wheel bug** · S `[FR]` — `--only-binary` pulls host `.so`/`.pyd`/`.dylib` into the mount (unloadable in-guest, baffling errors). Post-install: scan and hard-fail with the package name.
- [ ] **Redirect-on-failure error rewrites** · S `[FR]` — map ~15 common failures to actionable messages (`import pandas` → "use sqlite3 for tabular aggregation"). Highest-ROI usability change; models self-correct from these, not from raw tracebacks.
- [ ] **Tool description + schema redesign** · S `[FR]` — lead with the *unavailable* list, drop `args`/`stdin`, make `code`'s description carry the contract. (Draft in `notes.txt`.)
- [ ] 🟡 **Charts story** · M `[FR]` — if plotly doesn't land (spike), pick a fallback: emit SVG as text, unicode bars, or return structured data. Say so in the tool description.
- [ ] **Fix broken stdlib** · S `[FR]` — `tzdata` for `zoneinfo`; preopen a writable scratch dir + set `TMPDIR` for `tempfile`; document `threading`/`asyncio` as unsupported.
- [ ] 🟡 **Config hardening** · S — `wasm_threads(false)` (retires `RUSTSEC-2025-0118`), explicitly disable unused proposals, consider `consume_fuel` for per-tenant CPU accounting (fuel = billing; keep epochs for deadlines).

## P2 — Supply chain / provenance

- [ ] 🟡 **Pin + checksum runtimes** · S — `setup-runtimes.sh` curls `python-3.12.0.wasm` with no checksum and clones quickjs-ng unpinned at `main`. Pin tags, verify SHA-256, record versions in README.
- [ ] **CI supply-chain gate** · S — `cargo-audit` + `cargo-deny` as `make audit` in CI, plus Dependabot, so the version pin can't silently rot again.

## P3 — Performance & scale (only after criticals + isolation)

- [ ] **Snapshotting / pre-init** · L — the "microseconds" claim is wrong: you eliminated compilation, not CPython *init* (~tens–hundreds of ms). Wizer + `memory_init_cow` + pooling allocator = ~1ms restores. Bigger win than anything on the current roadmap. **Sequencing: upgrade wasmtime first** — pooling on 27.0.0 hits `RUSTSEC-2026-0088` (cross-instance leak).
- [ ] **Single-tier (wasm-only) design statement** · S `[FR]` — decisions made: **no Firecracker / gVisor / VM tier**; containment is a **standard Kubernetes pod** (runc). Every workload runs in wasm; a compiled library is supported *only if it compiles to wasm32-wasi and is baked into the trusted runtime*. Document this + density/p50 rationale in README.
- [ ] **Compiled-package super-binary build** · L `[FR]` — decided: bake libs into the trusted runtime by **compiling to wasm32-wasi** (path a). Anything that won't compile is unsupported + gets a redirect message (no VM fallback). Deliver: reproducible build pipeline (pinned toolchain, checksummed inputs), the target lib set (start: polars, numpy, plotly — validate in the spike), and an explicit supported-package list.
- [ ] **K8s warm-pod pool** · L — the containment + scaling layer. Multi-run: fewer warm pods, each running the in-pod process-per-run worker pool. Tune pods-per-node × workers-per-pod for density; keep pods warm to hide cold-start.

## Cross-cutting docs

- [ ] **`docs/threat-model.md`** · S — write down: adversary = agent code-exec; the three layers — capability boundary (strong/structural), memory boundary (depends on wasmtime N + patch SLA), and pod containment (runc + hardening, contains blast radius, not a memory boundary); the multi-run model and its consequence (in-pod process-per-run required, memory-boundary escape is cross-tenant); and the patch-cadence commitment. Every open question ("is shared `/packages` OK?") resolves against this.
- [ ] **Rewrite README security section** · S — two claims, two confidence levels; name the pinned wasmtime version; stop implying literal "no host filesystem" (two dirs are preopened) and cross-run isolation while single-process.

---

### Suggested order (from the review)
threat-model doc → wasmtime upgrade → 8-row tests (+sleep) → epoch/timeout fix → process-per-run → semaphore/limits → runtime spike → snapshotting/pooling → K8s.

---

## Glossary — jargon used above

**The wasm stack**
- **WebAssembly (wasm)** — a portable binary instruction format. We compile untrusted code to it and run it in a sandbox instead of running it natively.
- **linear memory** — the guest's single contiguous block of memory. The sandbox's job is to stop guest code reading/writing *outside* it; a bug that lets it do so is a "memory-boundary escape."
- **wasmtime** — the runtime (by the Bytecode Alliance) that executes our wasm. This is the big dependency we're pinned to and need to upgrade.
- **Cranelift** — wasmtime's optimizing compiler; turns wasm into native machine code. It's in our **TCB** because if it emits *wrong* code, the guest can escape linear memory. Most advisories that hit us live here.
- **WASI / WASIp1 / wasm32-wasi** — the standard set of "syscalls" (file, clock, random…) a wasm guest is allowed to call. `wasm32-wasi` is the compile target. WASIp1 is the older (preview1) API we use; several advisories are bugs in wasmtime's WASI *implementation*.
- **Store / Engine / Linker** — wasmtime concepts. **Engine** = shared compiler/config (one per process). **Store** = per-run state (we make a fresh one each run). **Linker** = the table of host functions the guest may call — leaving `socket()`/`fork()` *out* of it is our capability boundary.
- **epoch interruption / fuel** — two ways wasmtime interrupts guest code. **Epochs** = cheap periodic "time's up" checks (used for our timeout). **Fuel** = counts instructions executed (deterministic; better for billing). Neither can interrupt code stuck in a *host* call — that's the sleep-DoS bug.
- **trap** — a wasm execution fault (out-of-bounds, timeout, etc.) that cleanly aborts the guest. Good: it means the sandbox caught something.

**Isolation & OS**
- **tenant / cross-tenant** — a "tenant" is one caller's run. "Cross-tenant" = one run affecting or reading another. In multi-run pods this is the central risk.
- **sandbox escape** — untrusted guest code breaking out to read/run things it shouldn't (host memory, other tenants, the host OS).
- **TCB (Trusted Computing Base)** — the code you're forced to *trust* for security to hold. Smaller TCB = fewer places a bug is fatal. Cranelift + the C interpreter are our TCB.
- **capability boundary** — security by *not granting* an ability (the host function simply doesn't exist), vs. filtering/blocking it. Strong because there's nothing to bypass.
- **process-per-run** — running each program in its own OS process, so an escape is trapped in a throwaway process, not shared with siblings.
- **pod** — the Kubernetes unit that runs one or more containers. Our containment layer.
- **runc** — the standard Linux container runtime (what a normal pod uses). A namespace/cgroup boundary, *not* a memory boundary.
- **Firecracker / gVisor / microVM** — stronger VM-style isolation we've decided *not* to use. Firecracker = lightweight VMs; gVisor = a user-space kernel.
- **seccomp** — a Linux filter restricting which syscalls a process may make.
- **namespaces / netns** — Linux isolation of resources; a network namespace (`netns`) with nothing in it = no network.
- **RLIMIT_AS / RLIMIT_NPROC** — per-process kernel limits on address space (memory) and number of processes.
- **no_new_privs** — a flag that stops a process from gaining more privileges (e.g. via setuid).
- **automountServiceAccountToken: false** — Kubernetes setting that stops mounting cluster credentials into the pod, so an escaped guest can't steal them.
- **NetworkPolicy** — Kubernetes firewall rules; "deny-all egress" = the pod can't make outbound connections.

**Performance**
- **cold start / p50 / density** — cold start = time before user code runs. p50 = median latency. Density = how many concurrent runs fit per host. wasm's pitch is high density + low p50.
- **snapshotting / Wizer / pre-initialization** — start the interpreter once, freeze its memory *before* user code, and restore that image per run — turns ~100ms of CPython boot into ~1ms.
- **copy-on-write (CoW) / pooling allocator** — mechanisms that make restoring that frozen image cheap (an `mmap` instead of a fresh boot). `memory_init_cow` + pooling in wasmtime.

**Runtime candidates (the spike)**
- **CPython** — the standard Python interpreter, written in C. Battle-tested, huge stdlib (incl. `sqlite3`), but *not* memory-safe — its safety depends on the wasm sandbox holding.
- **QuickJS** — a small C JavaScript engine (our current JS runtime). Also unsafe C.
- **Boa / RustPython** — JS and Python interpreters written in **Rust** (memory-safe by construction — no linear memory to corrupt). Thinner than CPython/QuickJS.
- **Starlark** — a deterministic, Python-*like* config language (from Bazel) with no ambient I/O by design. Most locked-down option; not full Python.
- **polars / numpy / plotly** — popular data/plotting libraries. Compiled (not pure Python), so they only work here if compiled to wasm and baked into the super-binary.

**Security-tracking**
- **CVE (Common Vulnerabilities and Exposures)** — the industry-standard public catalog of known security flaws, each with an ID like `CVE-2024-1234`.
- **RUSTSEC-YYYY-NNNN** — an advisory ID in the Rust security database (like a CVE, but specifically for Rust crates). The ones cited are unpatched bugs reachable in our config.
- **LTS** — Long-Term Support; a release that gets security backports for an extended window (why we target wasmtime 36 LTS).
- **cargo-audit / cargo-deny** — tools that scan our dependency lockfile against RustSec so an outdated, vulnerable pin can't slip by unnoticed.
- **DoS** — Denial of Service; making the service unavailable (e.g. `time.sleep(3600)` × 512 pinning every worker).

**General / cross-cutting**
- **API (Application Programming Interface)** — the contract for calling the service (our `POST /run` endpoint, its inputs and outputs).
- **CI (Continuous Integration)** — automation that builds/tests every change; where the audit and capability checks would run to catch regressions.
- **SaaS (Software as a Service)** — software delivered as a hosted service to many customers; the "multi-tenant SaaS" tier means untrusted paying users sharing our infrastructure.
- **SLA (Service Level Agreement)** — a committed promise about a metric; here a "patch SLA" = "we patch a critical wasmtime advisory within N days."
- **FR (Functional Requirement)** — a behavior the product must have for callers (vs. hardening, which makes the same behavior safe). Tagged `[FR]` throughout.
- **HTTP 429 (Too Many Requests)** — the status code we return when at capacity, telling the caller to back off and retry.
- **stdlib (standard library)** — the modules that ship *with* Python (`json`, `sqlite3`, `statistics`…), no install needed. CPython's is large and battle-tested; mem-safe interpreters have thinner ones.
- **wheel** — a prebuilt Python package file (`.whl`). A "native wheel" contains compiled machine code (`.so`/`.pyd`) for a specific OS/CPU — which is why they don't load inside wasm (the `py-deps` bug).
- **mmap (memory-map)** — an OS call that maps a file/region into memory. Snapshot restore uses it to bring up a new instance by mapping a pre-initialized image instead of booting from scratch.
- **cgroup (control group)** — the Linux kernel feature Kubernetes uses to cap a pod/process's CPU and memory.

---

### What's a functional requirement vs. hardening?
`[FR]` items change what the product *does* for callers (limits contract, determinism, sessions, error redirects, schema, charts, stdlib fixes, tiering). Everything untagged makes the *same* behavior *safe* (upgrades, isolation, provenance, tests). In the agent code-exec tier, several usually-"non-functional" concerns (egress, determinism, quotas) are promoted to functional requirements because the caller is an untrusted model.
