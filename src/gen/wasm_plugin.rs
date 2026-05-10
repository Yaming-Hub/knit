//! WASM plugin support for custom generators.
//!
//! This module enables loading `.wasm` modules at runtime as generator plugins.
//! WASM plugins implement a simple ABI and are registered into the global
//! [`Registry`](crate::gen::plugin::Registry) alongside native Rust plugins.
//!
//! # ABI Contract (v1)
//!
//! A WASM module must export the following functions:
//!
//! | Export | Signature | Description |
//! |---|---|---|
//! | `knit_abi_version` | `() -> i32` | Must return `1` |
//! | `knit_name` | `() -> i32` | Returns ptr to name string (NUL-terminated) |
//! | `knit_name_len` | `() -> i32` | Returns byte length of name (excl. NUL) |
//! | `knit_output_type` | `() -> i32` | Arrow type: 0=Int64, 1=Float64, 2=Utf8, 3=Bool |
//! | `knit_create` | `(params_ptr: i32, params_len: i32) -> i32` | Create instance, return handle |
//! | `knit_generate` | `(handle: i32, seed_lo: i32, seed_hi: i32, count: i32) -> i32` | Generate JSON output, return ptr (guest-owned) |
//! | `knit_generate_len` | `(handle: i32) -> i32` | Length of last generate output |
//! | `knit_destroy` | `(handle: i32)` | Free instance |
//! | `knit_alloc` | `(size: i32) -> i32` | Allocate guest memory (for host→guest params) |
//! | `knit_free` | `(ptr: i32, size: i32)` | Free memory allocated by `knit_alloc` |
//! | `memory` | (memory export) | Linear memory |
//!
//! # Memory Ownership
//!
//! - **Params buffer**: Host allocates via `knit_alloc`, writes JSON, calls `knit_create`,
//!   then frees via `knit_free`. Guest must not hold the pointer past `knit_create` return.
//! - **Generate output**: Guest-owned. `knit_generate` returns a pointer to a buffer that
//!   the guest manages (e.g., a `thread_local` Vec). The host copies the data immediately
//!   and does **not** call `knit_free` on it. The guest may reuse/overwrite the buffer on
//!   the next `knit_generate` call.
//!
//! # Security
//!
//! v1 plugins run as **trusted local code** — no sandboxing or resource limits.
//! Only load WASM modules you trust.
//!
//! # Example
//!
//! ```bash
//! knit generate schema.toml -o out/ --plugin my_gen.wasm
//! ```

use std::path::Path;
use std::sync::Arc;

use arrow::array::ArrayRef;
use arrow::datatypes::DataType;
use rand::RngCore;

use crate::gen::context::GenContext;
use crate::gen::plugin::GeneratorPlugin;
use crate::gen::traits::FieldGenerator;

/// Current ABI version. WASM modules must return this from `knit_abi_version`.
const ABI_VERSION: i32 = 1;

/// Arrow output type codes used in the WASM ABI.
const TYPE_INT64: i32 = 0;
const TYPE_FLOAT64: i32 = 1;
const TYPE_UTF8: i32 = 2;
const TYPE_BOOLEAN: i32 = 3;

/// Errors from WASM plugin operations.
#[derive(Debug, thiserror::Error)]
pub enum WasmPluginError {
    /// WASM runtime error.
    #[error("wasmtime error: {0}")]
    Runtime(#[from] wasmtime::Error),

    /// Missing required export.
    #[error("WASM module missing required export: {0}")]
    MissingExport(String),

    /// ABI version mismatch.
    #[error("ABI version mismatch: expected {expected}, got {actual}")]
    AbiMismatch { expected: i32, actual: i32 },

    /// Unknown output type code.
    #[error("unknown output type code: {0}")]
    UnknownType(i32),

    /// Plugin creation error.
    #[error("plugin creation failed: {0}")]
    Creation(String),

    /// Generation error.
    #[error("generation failed: {0}")]
    Generation(String),
}

/// A generator plugin backed by a WASM module.
///
/// Holds the compiled module and engine. Each call to [`create`](GeneratorPlugin::create)
/// instantiates the module and returns a [`WasmFieldGenerator`].
pub struct WasmGeneratorPlugin {
    engine: wasmtime::Engine,
    module: wasmtime::Module,
    name: String,
    output_type: DataType,
}

impl WasmGeneratorPlugin {
    /// Load a WASM plugin from a file path.
    ///
    /// Validates the ABI version and reads the plugin name and output type.
    pub fn from_file(path: &Path) -> Result<Self, WasmPluginError> {
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::from_file(&engine, path)?;

        // Create a temporary instance to read metadata.
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[])?;

        // Check ABI version.
        let abi_version_fn = instance
            .get_typed_func::<(), i32>(&mut store, "knit_abi_version")
            .map_err(|_| WasmPluginError::MissingExport("knit_abi_version".into()))?;
        let version = abi_version_fn.call(&mut store, ())?;
        if version != ABI_VERSION {
            return Err(WasmPluginError::AbiMismatch {
                expected: ABI_VERSION,
                actual: version,
            });
        }

        // Read plugin name.
        let name = read_plugin_name(&instance, &mut store)?;

        // Read output type.
        let output_type_fn = instance
            .get_typed_func::<(), i32>(&mut store, "knit_output_type")
            .map_err(|_| WasmPluginError::MissingExport("knit_output_type".into()))?;
        let type_code = output_type_fn.call(&mut store, ())?;
        let output_type = type_code_to_arrow(type_code)?;

        Ok(Self {
            engine,
            module,
            name,
            output_type,
        })
    }
}

impl GeneratorPlugin for WasmGeneratorPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn create(
        &self,
        params: &std::collections::BTreeMap<String, crate::core::Value>,
    ) -> Result<Box<dyn FieldGenerator>, String> {
        let mut store = wasmtime::Store::new(&self.engine, ());
        let instance = wasmtime::Instance::new(&mut store, &self.module, &[])
            .map_err(|e| format!("WASM instantiation failed: {e}"))?;

        // Serialize params to JSON and write into guest memory.
        let params_json = serde_json::to_string(params)
            .map_err(|e| format!("failed to serialize params: {e}"))?;

        let handle = if params_json.len() > 2 {
            // Non-empty params (not just "{}")
            let params_bytes = params_json.as_bytes();
            let guest_ptr = call_knit_alloc(&instance, &mut store, params_bytes.len() as i32)
                .map_err(|e| format!("knit_alloc failed: {e}"))?;

            // Write params into guest memory.
            let memory = instance
                .get_memory(&mut store, "memory")
                .ok_or_else(|| "WASM module does not export 'memory'".to_string())?;
            memory
                .write(&mut store, guest_ptr as usize, params_bytes)
                .map_err(|e| format!("failed to write params to guest memory: {e}"))?;

            let create_fn = instance
                .get_typed_func::<(i32, i32), i32>(&mut store, "knit_create")
                .map_err(|_| "missing export: knit_create".to_string())?;
            let h = create_fn
                .call(&mut store, (guest_ptr, params_bytes.len() as i32))
                .map_err(|e| format!("knit_create trapped: {e}"))?;

            // Free the params buffer.
            let _ = call_knit_free(&instance, &mut store, guest_ptr, params_bytes.len() as i32);
            h
        } else {
            // Empty params — pass null.
            let create_fn = instance
                .get_typed_func::<(i32, i32), i32>(&mut store, "knit_create")
                .map_err(|_| "missing export: knit_create".to_string())?;
            create_fn
                .call(&mut store, (0, 0))
                .map_err(|e| format!("knit_create trapped: {e}"))?
        };

        if handle < 0 {
            return Err(format!("knit_create returned error handle: {handle}"));
        }

        Ok(Box::new(WasmFieldGenerator {
            instance,
            store: std::sync::Mutex::new(store),
            handle,
            output_type: self.output_type.clone(),
        }))
    }
}

/// A field generator backed by a WASM module instance.
///
/// Each `WasmFieldGenerator` owns its own `Instance` and `Store`, so multiple
/// generators (from different fields or partitions) can run concurrently.
pub struct WasmFieldGenerator {
    instance: wasmtime::Instance,
    store: std::sync::Mutex<wasmtime::Store<()>>,
    handle: i32,
    output_type: DataType,
}

// Safety: wasmtime::Store is Send but not Sync; we wrap it in a Mutex.
unsafe impl Sync for WasmFieldGenerator {}

impl FieldGenerator for WasmFieldGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, _ctx: &GenContext) -> ArrayRef {
        let seed = rng.next_u64();
        let seed_lo = seed as i32;
        let seed_hi = (seed >> 32) as i32;

        let mut store = self.store.lock().expect("WASM store lock poisoned");

        // Call knit_generate.
        let result = (|| -> Result<ArrayRef, String> {
            let generate_fn = self
                .instance
                .get_typed_func::<(i32, i32, i32, i32), i32>(&mut *store, "knit_generate")
                .map_err(|_| "missing export: knit_generate".to_string())?;

            let result_ptr = generate_fn
                .call(&mut *store, (self.handle, seed_lo, seed_hi, count as i32))
                .map_err(|e| format!("knit_generate trapped: {e}"))?;

            if result_ptr < 0 {
                return Err(format!("knit_generate returned error: {result_ptr}"));
            }

            // Read result length.
            let len_fn = self
                .instance
                .get_typed_func::<(i32,), i32>(&mut *store, "knit_generate_len")
                .map_err(|_| "missing export: knit_generate_len".to_string())?;
            let result_len = len_fn
                .call(&mut *store, (self.handle,))
                .map_err(|e| format!("knit_generate_len trapped: {e}"))?;

            if result_len <= 0 {
                return Err(format!("knit_generate_len returned {result_len}"));
            }

            // Cap result length to prevent host OOM from malicious/buggy plugins.
            const MAX_RESULT_BYTES: i32 = 256 * 1024 * 1024; // 256 MB
            if result_len > MAX_RESULT_BYTES {
                return Err(format!(
                    "knit_generate_len returned {result_len} bytes (max {MAX_RESULT_BYTES})"
                ));
            }

            // Read JSON from guest memory.
            let memory = self
                .instance
                .get_memory(&mut *store, "memory")
                .ok_or_else(|| "WASM module does not export 'memory'".to_string())?;

            let mut buf = vec![0u8; result_len as usize];
            memory
                .read(&*store, result_ptr as usize, &mut buf)
                .map_err(|e| format!("failed to read guest memory: {e}"))?;

            // Note: the host does NOT free the result buffer — the guest owns it
            // and may reuse it across calls (e.g., via a thread-local Vec).

            let json_str = std::str::from_utf8(&buf)
                .map_err(|e| format!("invalid UTF-8 from WASM: {e}"))?;

            // Parse JSON array into Arrow ArrayRef.
            parse_json_to_array(json_str, &self.output_type, count)
        })();

        match result {
            Ok(arr) => arr,
            Err(e) => {
                tracing::error!(error = %e, "WASM generator failed — returning nulls");
                null_array(&self.output_type, count)
            }
        }
    }

    fn output_type(&self) -> DataType {
        self.output_type.clone()
    }
}

impl Drop for WasmFieldGenerator {
    fn drop(&mut self) {
        if let Ok(mut store) = self.store.lock() {
            if let Ok(destroy_fn) =
                self.instance
                    .get_typed_func::<(i32,), ()>(&mut *store, "knit_destroy")
            {
                let _ = destroy_fn.call(&mut *store, (self.handle,));
            }
        }
    }
}

// ── Helper functions ────────────────────────────────────────────────────

fn read_plugin_name(
    instance: &wasmtime::Instance,
    store: &mut wasmtime::Store<()>,
) -> Result<String, WasmPluginError> {
    let name_fn = instance
        .get_typed_func::<(), i32>(&mut *store, "knit_name")
        .map_err(|_| WasmPluginError::MissingExport("knit_name".into()))?;
    let name_len_fn = instance
        .get_typed_func::<(), i32>(&mut *store, "knit_name_len")
        .map_err(|_| WasmPluginError::MissingExport("knit_name_len".into()))?;

    let name_ptr = name_fn.call(&mut *store, ())?;
    let name_len = name_len_fn.call(&mut *store, ())?;

    if name_len <= 0 || name_ptr < 0 {
        return Err(WasmPluginError::Creation("invalid name pointer/length".into()));
    }

    let memory = instance
        .get_memory(&mut *store, "memory")
        .ok_or_else(|| WasmPluginError::MissingExport("memory".into()))?;

    let mut buf = vec![0u8; name_len as usize];
    memory
        .read(&*store, name_ptr as usize, &mut buf)
        .map_err(|e| WasmPluginError::Creation(format!("failed to read plugin name: {e}")))?;

    String::from_utf8(buf)
        .map_err(|e| WasmPluginError::Creation(format!("invalid UTF-8 in plugin name: {e}")))
}

fn type_code_to_arrow(code: i32) -> Result<DataType, WasmPluginError> {
    match code {
        TYPE_INT64 => Ok(DataType::Int64),
        TYPE_FLOAT64 => Ok(DataType::Float64),
        TYPE_UTF8 => Ok(DataType::Utf8),
        TYPE_BOOLEAN => Ok(DataType::Boolean),
        _ => Err(WasmPluginError::UnknownType(code)),
    }
}

fn call_knit_alloc(
    instance: &wasmtime::Instance,
    store: &mut wasmtime::Store<()>,
    size: i32,
) -> Result<i32, wasmtime::Error> {
    let alloc_fn = instance.get_typed_func::<(i32,), i32>(&mut *store, "knit_alloc")?;
    alloc_fn.call(&mut *store, (size,))
}

fn call_knit_free(
    instance: &wasmtime::Instance,
    store: &mut wasmtime::Store<()>,
    ptr: i32,
    size: i32,
) -> Result<(), wasmtime::Error> {
    let free_fn = instance.get_typed_func::<(i32, i32), ()>(&mut *store, "knit_free")?;
    free_fn.call(&mut *store, (ptr, size))
}

fn parse_json_to_array(
    json_str: &str,
    data_type: &DataType,
    expected_count: usize,
) -> Result<ArrayRef, String> {
    let values: Vec<serde_json::Value> =
        serde_json::from_str(json_str).map_err(|e| format!("invalid JSON from WASM: {e}"))?;

    if values.len() != expected_count {
        return Err(format!(
            "WASM returned {} values, expected {}",
            values.len(),
            expected_count
        ));
    }

    match data_type {
        DataType::Int64 => {
            let arr: arrow::array::Int64Array = values
                .iter()
                .map(|v| v.as_i64())
                .collect();
            Ok(Arc::new(arr))
        }
        DataType::Float64 => {
            let arr: arrow::array::Float64Array = values
                .iter()
                .map(|v| v.as_f64())
                .collect();
            Ok(Arc::new(arr))
        }
        DataType::Utf8 => {
            let arr: arrow::array::StringArray = values
                .iter()
                .map(|v| v.as_str())
                .collect();
            Ok(Arc::new(arr))
        }
        DataType::Boolean => {
            let arr: arrow::array::BooleanArray = values
                .iter()
                .map(|v| v.as_bool())
                .collect();
            Ok(Arc::new(arr))
        }
        _ => Err(format!("unsupported WASM output type: {data_type}")),
    }
}

fn null_array(data_type: &DataType, count: usize) -> ArrayRef {
    match data_type {
        DataType::Int64 => {
            let arr: arrow::array::Int64Array = vec![None; count].into_iter().collect();
            Arc::new(arr)
        }
        DataType::Float64 => {
            let arr: arrow::array::Float64Array = vec![None; count].into_iter().collect();
            Arc::new(arr)
        }
        DataType::Utf8 => {
            let arr: arrow::array::StringArray =
                vec![None::<&str>; count].into_iter().collect();
            Arc::new(arr)
        }
        DataType::Boolean => {
            let arr: arrow::array::BooleanArray =
                vec![None; count].into_iter().collect();
            Arc::new(arr)
        }
        _ => {
            let arr: arrow::array::Int64Array = vec![None; count].into_iter().collect();
            Arc::new(arr)
        }
    }
}

/// Load all WASM plugins from a directory.
///
/// Scans the directory for `.wasm` files and loads each as a
/// [`WasmGeneratorPlugin`]. Successfully loaded plugins are registered in the
/// global [`Registry`](crate::gen::plugin::Registry).
///
/// Returns errors for individual files that fail to load but continues
/// processing remaining files.
pub fn load_wasm_plugins_from_dir(dir: &Path) -> Result<Vec<String>, WasmPluginError> {
    let mut loaded = Vec::new();
    let mut seen_names = std::collections::HashSet::new();

    let entries = std::fs::read_dir(dir).map_err(|e| {
        WasmPluginError::Creation(format!("failed to read plugin directory: {e}"))
    })?;

    for entry in entries {
        let entry = entry.map_err(|e| {
            WasmPluginError::Creation(format!("failed to read directory entry: {e}"))
        })?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("wasm") {
            match load_wasm_plugin(&path, &mut seen_names) {
                Ok(name) => loaded.push(name),
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "failed to load WASM plugin");
                }
            }
        }
    }

    Ok(loaded)
}

/// Load a single WASM plugin file and register it.
///
/// Returns an error if a plugin with the same name is already loaded from a
/// file (duplicate detection for file-loaded plugins only).
pub fn load_wasm_plugin(
    path: &Path,
    seen_names: &mut std::collections::HashSet<String>,
) -> Result<String, WasmPluginError> {
    let plugin = WasmGeneratorPlugin::from_file(path)?;
    let name = plugin.name().to_string();

    if !seen_names.insert(name.clone()) {
        return Err(WasmPluginError::Creation(format!(
            "duplicate WASM plugin name: '{name}'"
        )));
    }

    tracing::info!(name = %name, path = %path.display(), "loaded WASM generator plugin");
    crate::gen::plugin::registry().register(Box::new(plugin));
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_code_mapping() {
        assert_eq!(type_code_to_arrow(TYPE_INT64).unwrap(), DataType::Int64);
        assert_eq!(type_code_to_arrow(TYPE_FLOAT64).unwrap(), DataType::Float64);
        assert_eq!(type_code_to_arrow(TYPE_UTF8).unwrap(), DataType::Utf8);
        assert_eq!(type_code_to_arrow(TYPE_BOOLEAN).unwrap(), DataType::Boolean);
        assert!(type_code_to_arrow(99).is_err());
    }

    #[test]
    fn parse_json_int64_array() {
        let arr = parse_json_to_array("[1, 2, 3]", &DataType::Int64, 3).unwrap();
        assert_eq!(arr.len(), 3);
        let int_arr = arr
            .as_any()
            .downcast_ref::<arrow::array::Int64Array>()
            .unwrap();
        assert_eq!(int_arr.value(0), 1);
        assert_eq!(int_arr.value(1), 2);
        assert_eq!(int_arr.value(2), 3);
    }

    #[test]
    fn parse_json_float64_array() {
        let arr = parse_json_to_array("[1.5, 2.5]", &DataType::Float64, 2).unwrap();
        let f_arr = arr
            .as_any()
            .downcast_ref::<arrow::array::Float64Array>()
            .unwrap();
        assert!((f_arr.value(0) - 1.5).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_json_utf8_array() {
        let arr =
            parse_json_to_array(r#"["hello", "world"]"#, &DataType::Utf8, 2).unwrap();
        let s_arr = arr
            .as_any()
            .downcast_ref::<arrow::array::StringArray>()
            .unwrap();
        assert_eq!(s_arr.value(0), "hello");
        assert_eq!(s_arr.value(1), "world");
    }

    #[test]
    fn parse_json_boolean_array() {
        let arr =
            parse_json_to_array("[true, false, true]", &DataType::Boolean, 3).unwrap();
        let b_arr = arr
            .as_any()
            .downcast_ref::<arrow::array::BooleanArray>()
            .unwrap();
        assert!(b_arr.value(0));
        assert!(!b_arr.value(1));
    }

    #[test]
    fn parse_json_count_mismatch() {
        let result = parse_json_to_array("[1, 2]", &DataType::Int64, 3);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("expected 3"));
    }

    #[test]
    fn null_array_produces_correct_length() {
        let arr = null_array(&DataType::Int64, 5);
        assert_eq!(arr.len(), 5);
        assert_eq!(arr.null_count(), 5);
    }
}
