//! The wasmtime-backed execution engine.
//!
//! Each supported language maps to a precompiled `.wasm` module (CPython for
//! Python, QuickJS for JS/TS). Modules are compiled once at startup and held in
//! `Arc`s — this is the local analog of a "warm pod": no per-request compile
//! cost, just a fresh `Store` + instance per run for isolation.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use wasmtime::{Config, Engine, Module, Store, StoreLimits, StoreLimitsBuilder, Trap};
use wasmtime_wasi::pipe::{MemoryInputPipe, MemoryOutputPipe};
use wasmtime_wasi::preview1::{self, WasiP1Ctx};
use wasmtime_wasi::{DirPerms, FilePerms, I32Exit, WasiCtxBuilder};

use crate::api::{Language, RunResponse};

/// Per-run resource limits.
#[derive(Clone, Copy)]
pub struct Limits {
    pub default_timeout_ms: u64,
    pub max_timeout_ms: u64,
    /// Linear-memory cap for the guest, in bytes.
    pub memory_bytes: usize,
    /// Max bytes captured from stdout/stderr each (excess is truncated).
    pub output_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            default_timeout_ms: 5_000,
            max_timeout_ms: 30_000,
            memory_bytes: 256 * 1024 * 1024,
            output_bytes: 1024 * 1024,
        }
    }
}

/// Data threaded through the `Store`: the WASI context plus the memory limiter.
struct HostState {
    wasi: WasiP1Ctx,
    limits: StoreLimits,
}

/// A warm, ready-to-run sandbox. Cheap to clone (everything is `Arc`/handle).
#[derive(Clone)]
pub struct Sandbox {
    engine: Engine,
    python: Option<Arc<Module>>,
    qjs: Option<Arc<Module>>,
    limits: Limits,
}

/// How a given language is turned into "a wasm module + argv + a source file".
struct Plan {
    module: Arc<Module>,
    /// (filename inside the sandbox dir, file contents)
    source_file: (String, String),
    /// argv passed to the guest (argv[0] is the program name).
    argv: Vec<String>,
}

impl Sandbox {
    /// Compile the available runtime modules up front.
    pub fn new(
        python_wasm: Option<&str>,
        qjs_wasm: Option<&str>,
        limits: Limits,
    ) -> Result<Self> {
        let mut config = Config::new();
        config.epoch_interruption(true);
        // Cache compiled artifacts across runs implicitly via the held Module.
        let engine = Engine::new(&config).context("failed to create wasm engine")?;

        let compile = |path: &str| -> Result<Arc<Module>> {
            let m = Module::from_file(&engine, path)
                .with_context(|| format!("failed to compile module: {path}"))?;
            Ok(Arc::new(m))
        };

        let python = match python_wasm {
            Some(p) => Some(compile(p)?),
            None => None,
        };
        let qjs = match qjs_wasm {
            Some(p) => Some(compile(p)?),
            None => None,
        };

        Ok(Self {
            engine,
            python,
            qjs,
            limits,
        })
    }

    pub fn supports(&self, lang: Language) -> bool {
        match lang {
            Language::Python => self.python.is_some(),
            Language::Javascript | Language::Typescript => self.qjs.is_some(),
        }
    }

    /// Build the execution plan for a language, transpiling TS if needed.
    fn plan(&self, lang: Language, code: &str, args: &[String]) -> Result<Plan> {
        match lang {
            Language::Python => {
                let module = self
                    .python
                    .clone()
                    .ok_or_else(|| anyhow!("python runtime not available"))?;
                let mut argv = vec!["python".to_string(), "/sandbox/main.py".to_string()];
                argv.extend_from_slice(args);
                Ok(Plan {
                    module,
                    source_file: ("main.py".to_string(), code.to_string()),
                    argv,
                })
            }
            Language::Javascript | Language::Typescript => {
                let module = self
                    .qjs
                    .clone()
                    .ok_or_else(|| anyhow!("javascript runtime not available"))?;
                let js = if lang == Language::Typescript {
                    crate::transpile::typescript_to_js(code)
                        .context("typescript transpilation failed")?
                } else {
                    code.to_string()
                };
                let mut argv = vec!["qjs".to_string(), "/sandbox/main.js".to_string()];
                argv.extend_from_slice(args);
                Ok(Plan {
                    module,
                    source_file: ("main.js".to_string(), js),
                    argv,
                })
            }
        }
    }

    /// Execute code and return a fully-formed response. Never panics on guest
    /// misbehavior — traps and timeouts are reported in the response.
    pub fn execute(
        &self,
        lang: Language,
        code: &str,
        stdin: Option<&str>,
        args: &[String],
        timeout_ms: Option<u64>,
    ) -> RunResponse {
        let start = Instant::now();

        // Setup errors (e.g. TS transpile failure) return a response with `error`.
        let plan = match self.plan(lang, code, args) {
            Ok(p) => p,
            Err(e) => {
                return RunResponse {
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: None,
                    duration_ms: start.elapsed().as_millis(),
                    timed_out: false,
                    trapped: false,
                    error: Some(format!("{e:#}")),
                }
            }
        };

        match self.run_module(plan, stdin, timeout_ms, start) {
            Ok(resp) => resp,
            Err(e) => RunResponse {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
                duration_ms: start.elapsed().as_millis(),
                timed_out: false,
                trapped: true,
                error: Some(format!("internal execution error: {e:#}")),
            },
        }
    }

    fn run_module(
        &self,
        plan: Plan,
        stdin: Option<&str>,
        timeout_ms: Option<u64>,
        start: Instant,
    ) -> Result<RunResponse> {
        let timeout = timeout_ms
            .unwrap_or(self.limits.default_timeout_ms)
            .min(self.limits.max_timeout_ms)
            .max(1);

        // Write the user's source into an isolated temp dir, mounted read-only.
        let dir = tempfile::tempdir().context("failed to create sandbox dir")?;
        let (fname, contents) = &plan.source_file;
        std::fs::write(dir.path().join(fname), contents).context("failed to stage source")?;

        let stdout = MemoryOutputPipe::new(self.limits.output_bytes);
        let stderr = MemoryOutputPipe::new(self.limits.output_bytes);

        let mut builder = WasiCtxBuilder::new();
        builder
            .stdout(stdout.clone())
            .stderr(stderr.clone())
            .args(&plan.argv)
            .preopened_dir(dir.path(), "/sandbox", DirPerms::READ, FilePerms::READ)?;
        if let Some(input) = stdin {
            builder.stdin(MemoryInputPipe::new(input.to_string().into_bytes()));
        }
        let wasi = builder.build_p1();

        let host = HostState {
            wasi,
            limits: StoreLimitsBuilder::new()
                .memory_size(self.limits.memory_bytes)
                .build(),
        };

        let mut store = Store::new(&self.engine, host);
        store.limiter(|h: &mut HostState| &mut h.limits);
        // Trap once the epoch is bumped past this deadline.
        store.set_epoch_deadline(1);

        let mut linker = wasmtime::Linker::new(&self.engine);
        preview1::add_to_linker_sync(&mut linker, |h: &mut HostState| &mut h.wasi)?;

        let instance = linker
            .instantiate(&mut store, &plan.module)
            .context("failed to instantiate module")?;
        let func = instance
            .get_typed_func::<(), ()>(&mut store, "_start")
            .context("module has no _start export")?;

        // Wall-clock watchdog: bump the epoch once the deadline passes, unless
        // the run finishes first and signals us to stop.
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let engine = self.engine.clone();
        let watchdog = std::thread::spawn(move || {
            if done_rx.recv_timeout(Duration::from_millis(timeout)).is_err() {
                engine.increment_epoch();
            }
        });

        let call_result = func.call(&mut store, ());

        // Tell the watchdog we're done and wait for it to exit.
        let _ = done_tx.send(());
        let _ = watchdog.join();

        let mut timed_out = false;
        let mut trapped = false;
        let mut exit_code = None;
        let mut extra_stderr = String::new();

        match call_result {
            Ok(()) => exit_code = Some(0),
            Err(e) => {
                if let Some(exit) = e.downcast_ref::<I32Exit>() {
                    // Normal exit via proc_exit.
                    exit_code = Some(exit.0);
                } else if let Some(trap) = e.downcast_ref::<Trap>() {
                    if *trap == Trap::Interrupt {
                        timed_out = true;
                    } else {
                        trapped = true;
                        extra_stderr = format!("\n[trap] {trap}");
                    }
                } else {
                    trapped = true;
                    extra_stderr = format!("\n[error] {e:#}");
                }
            }
        }

        let stdout = String::from_utf8_lossy(&stdout.contents()).to_string();
        let mut stderr = String::from_utf8_lossy(&stderr.contents()).to_string();
        stderr.push_str(&extra_stderr);

        Ok(RunResponse {
            stdout,
            stderr,
            exit_code,
            duration_ms: start.elapsed().as_millis(),
            timed_out,
            trapped,
            error: None,
        })
    }
}
