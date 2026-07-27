//! TypeScript -> JavaScript transpilation.
//!
//! Transpilation is a *trusted, host-side* step: it only parses and strips
//! types, it does not execute user code. The resulting JS is what actually runs
//! inside the wasm sandbox. Gated behind the `typescript` feature so the base
//! build stays lean.

use anyhow::Result;

#[cfg(feature = "typescript")]
pub fn typescript_to_js(source: &str) -> Result<String> {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use swc_common::errors::{EmitterWriter, Handler};
    use swc_common::sync::Lrc;
    use swc_common::{Globals, SourceMap, GLOBALS};
    use swc_fast_ts_strip::{operate, Mode, Options};

    // A Write sink that accumulates emitted diagnostics so we can return a
    // useful compile error to the caller instead of a generic message.
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);
    impl Write for SharedBuf {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let cm: Lrc<SourceMap> = Default::default();
    let diagnostics = Arc::new(Mutex::new(Vec::<u8>::new()));
    let emitter = EmitterWriter::new(
        Box::new(SharedBuf(diagnostics.clone())),
        Some(cm.clone()),
        false,
        false,
    );
    let handler = Handler::with_emitter(true, false, Box::new(emitter));

    let options = Options {
        filename: Some("main.ts".to_string()),
        // Transform (not strip-only) so enums, namespaces, parameter
        // properties, etc. are lowered to real JS rather than rejected.
        mode: Mode::Transform,
        ..Default::default()
    };

    // `operate` uses `Mark::new()` internally, which requires a GLOBALS scope.
    let globals = Globals::default();
    let result = GLOBALS.set(&globals, || operate(&cm, &handler, source.to_string(), options));

    match result {
        Ok(out) => Ok(out.code),
        Err(e) => {
            let diag = String::from_utf8_lossy(&diagnostics.lock().unwrap())
                .trim()
                .to_string();
            if diag.is_empty() {
                anyhow::bail!("{e}")
            } else {
                anyhow::bail!("{diag}")
            }
        }
    }
}

#[cfg(not(feature = "typescript"))]
pub fn typescript_to_js(_source: &str) -> Result<String> {
    anyhow::bail!(
        "TypeScript support is not compiled in; rebuild with `--features typescript`"
    )
}
