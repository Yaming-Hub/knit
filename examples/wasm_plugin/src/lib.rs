//! Example Knit WASM generator plugin.
//!
//! This plugin generates random floating-point values in a configurable range.
//! It demonstrates the Knit WASM ABI v1 contract.
//!
//! # Build
//!
//! ```bash
//! rustup target add wasm32-wasip1
//! cargo build --target wasm32-wasip1 --release
//! ```
//!
//! # Usage
//!
//! ```bash
//! knit generate schema.toml -o out/ \
//!   --plugin target/wasm32-wasip1/release/knit_example_wasm_plugin.wasm
//! ```
//!
//! In the schema:
//! ```toml
//! [[entities.fields]]
//! name = "score"
//! data_type = "float"
//! [entities.fields.generator]
//! type = "plugin"
//! name = "random_float"
//! [entities.fields.generator.params]
//! min = 0.0
//! max = 100.0
//! ```

use std::cell::RefCell;
use std::collections::HashMap;

// ── Plugin state ────────────────────────────────────────────────────

struct GeneratorState {
    min: f64,
    max: f64,
}

thread_local! {
    static INSTANCES: RefCell<HashMap<i32, GeneratorState>> = RefCell::new(HashMap::new());
    static NEXT_HANDLE: RefCell<i32> = RefCell::new(1);
    static LAST_OUTPUT: RefCell<Vec<u8>> = RefCell::new(Vec::new());
}

// ── ABI exports ─────────────────────────────────────────────────────

const PLUGIN_NAME: &str = "random_float";

/// ABI version — must return 1 for Knit WASM ABI v1.
#[unsafe(no_mangle)]
pub extern "C" fn knit_abi_version() -> i32 {
    1
}

/// Pointer to the plugin name string.
#[unsafe(no_mangle)]
pub extern "C" fn knit_name() -> i32 {
    PLUGIN_NAME.as_ptr() as i32
}

/// Length of the plugin name string.
#[unsafe(no_mangle)]
pub extern "C" fn knit_name_len() -> i32 {
    PLUGIN_NAME.len() as i32
}

/// Output type code: 1 = Float64.
#[unsafe(no_mangle)]
pub extern "C" fn knit_output_type() -> i32 {
    1 // Float64
}

/// Create a generator instance from JSON parameters.
///
/// Reads `min` and `max` from the params JSON. Defaults to [0.0, 1.0].
#[unsafe(no_mangle)]
pub extern "C" fn knit_create(params_ptr: i32, params_len: i32) -> i32 {
    let (min, max) = if params_len > 0 {
        let params_bytes =
            unsafe { std::slice::from_raw_parts(params_ptr as *const u8, params_len as usize) };
        if let Ok(params_str) = std::str::from_utf8(params_bytes) {
            if let Ok(map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(
                params_str,
            ) {
                let min = map
                    .get("min")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let max = map
                    .get("max")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(1.0);
                (min, max)
            } else {
                (0.0, 1.0)
            }
        } else {
            (0.0, 1.0)
        }
    } else {
        (0.0, 1.0)
    };

    let handle = NEXT_HANDLE.with(|h| {
        let mut h = h.borrow_mut();
        let handle = *h;
        *h += 1;
        handle
    });

    INSTANCES.with(|instances| {
        instances
            .borrow_mut()
            .insert(handle, GeneratorState { min, max });
    });

    handle
}

/// Generate `count` random float values using the provided seed.
///
/// The seed is split across `seed_lo` (low 32 bits) and `seed_hi` (high 32 bits).
/// Returns a pointer to the JSON output in linear memory.
#[unsafe(no_mangle)]
pub extern "C" fn knit_generate(handle: i32, seed_lo: i32, seed_hi: i32, count: i32) -> i32 {
    let seed = (seed_lo as u64) | ((seed_hi as u64) << 32);

    let (min, max) = INSTANCES.with(|instances| {
        let instances = instances.borrow();
        if let Some(state) = instances.get(&handle) {
            (state.min, state.max)
        } else {
            (0.0, 1.0)
        }
    });

    // Simple xoshiro-style PRNG for determinism.
    let mut state = seed;
    let mut values = Vec::with_capacity(count as usize);
    let range = max - min;

    for _ in 0..count {
        // xorshift64
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let uniform = (state as f64) / (u64::MAX as f64); // [0, 1)
        values.push(min + uniform * range);
    }

    // Serialize to JSON.
    let json = serde_json::to_string(&values).unwrap_or_else(|_| "[]".to_string());
    let json_bytes = json.into_bytes();

    LAST_OUTPUT.with(|output| {
        let mut output = output.borrow_mut();
        *output = json_bytes;
        output.as_ptr() as i32
    })
}

/// Length of the last generated output.
#[unsafe(no_mangle)]
pub extern "C" fn knit_generate_len(_handle: i32) -> i32 {
    LAST_OUTPUT.with(|output| output.borrow().len() as i32)
}

/// Destroy a generator instance.
#[unsafe(no_mangle)]
pub extern "C" fn knit_destroy(handle: i32) {
    INSTANCES.with(|instances| {
        instances.borrow_mut().remove(&handle);
    });
}

/// Allocate memory in guest linear memory.
#[unsafe(no_mangle)]
pub extern "C" fn knit_alloc(size: i32) -> i32 {
    let layout = std::alloc::Layout::from_size_align(size as usize, 8).unwrap();
    let ptr = unsafe { std::alloc::alloc(layout) };
    ptr as i32
}

/// Free memory in guest linear memory.
#[unsafe(no_mangle)]
pub extern "C" fn knit_free(ptr: i32, size: i32) {
    let layout = std::alloc::Layout::from_size_align(size as usize, 8).unwrap();
    unsafe { std::alloc::dealloc(ptr as *mut u8, layout) };
}
