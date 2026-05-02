//! Plugin registry for custom generator extensions.
//!
//! This module provides a [`Registry`] that allows external code to register
//! custom [`FieldGenerator`] implementations at runtime. Registered plugins
//! can be discovered by name via [`Registry::find`].
//!
//! **Note:** The generation engine does not yet automatically consult the
//! registry for unknown generator types. Integration with the schema parser
//! and plan compiler is planned for a future release. Currently, plugins must
//! be manually instantiated after lookup.
//!
//! # Usage
//!
//! ```ignore
//! use knit_gen::plugin::{registry, GeneratorPlugin};
//!
//! struct MyPlugin;
//! impl GeneratorPlugin for MyPlugin {
//!     fn name(&self) -> &str { "my_custom" }
//!     fn create(&self, params: &HashMap<String, String>) -> Box<dyn FieldGenerator> { ... }
//! }
//!
//! registry().register(Box::new(MyPlugin));
//! ```

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use crate::traits::FieldGenerator;

/// A trait for custom generator plugins that can be registered at runtime.
///
/// Implementors provide a unique name (used in schema `type = "..."` fields)
/// and a factory method that produces a [`FieldGenerator`] from string parameters.
pub trait GeneratorPlugin: Send + Sync {
    /// Unique name for this generator type (used in schema `type = "..."`).
    fn name(&self) -> &str;

    /// Create a [`FieldGenerator`] instance from the given parameters.
    fn create(&self, params: &HashMap<String, String>) -> Box<dyn FieldGenerator>;
}

/// Thread-safe registry of [`GeneratorPlugin`] instances.
///
/// Plugins are stored in insertion order and looked up by name.
/// The registry is globally accessible via [`registry()`].
pub struct Registry {
    plugins: RwLock<Vec<Box<dyn GeneratorPlugin>>>,
}

impl Registry {
    /// Create a new, empty registry.
    fn new() -> Self {
        Self {
            plugins: RwLock::new(Vec::new()),
        }
    }

    /// Register a custom generator plugin.
    ///
    /// If a plugin with the same name already exists, the new one replaces it.
    pub fn register(&self, plugin: Box<dyn GeneratorPlugin>) {
        let mut plugins = self.plugins.write().expect("plugin registry lock poisoned");
        let name = plugin.name().to_string();
        plugins.retain(|p| p.name() != name);
        tracing::info!(plugin = %name, "registered generator plugin");
        plugins.push(plugin);
    }

    /// Look up a registered plugin by name.
    ///
    /// Returns `None` if no plugin with the given name has been registered.
    pub fn find(&self, name: &str) -> Option<Box<dyn FieldGenerator>> {
        let plugins = self.plugins.read().expect("plugin registry lock poisoned");
        let params = HashMap::new();
        plugins.iter().find(|p| p.name() == name).map(|p| p.create(&params))
    }

    /// Look up a plugin by name and create a generator with the given parameters.
    ///
    /// Returns `None` if no plugin with the given name has been registered.
    pub fn create(
        &self,
        name: &str,
        params: &HashMap<String, String>,
    ) -> Option<Box<dyn FieldGenerator>> {
        let plugins = self.plugins.read().expect("plugin registry lock poisoned");
        plugins.iter().find(|p| p.name() == name).map(|p| p.create(params))
    }

    /// List the names of all registered plugins.
    pub fn registered_names(&self) -> Vec<String> {
        let plugins = self.plugins.read().expect("plugin registry lock poisoned");
        plugins.iter().map(|p| p.name().to_string()).collect()
    }
}

/// Global plugin registry singleton.
static REGISTRY: OnceLock<Registry> = OnceLock::new();

/// Access the global plugin registry.
///
/// The registry is lazily initialised on first access and lives for the
/// duration of the process.
pub fn registry() -> &'static Registry {
    REGISTRY.get_or_init(Registry::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{ArrayRef, Int64Array};
    use arrow::datatypes::DataType;
    use rand::RngCore;
    use std::sync::Arc;

    use crate::context::GenContext;

    /// A trivial generator that always returns 42.
    struct FortyTwoGenerator;

    impl FieldGenerator for FortyTwoGenerator {
        fn generate(&self, _rng: &mut dyn RngCore, count: usize, _ctx: &GenContext) -> ArrayRef {
            let arr: Int64Array = vec![42i64; count].into();
            Arc::new(arr)
        }
        fn output_type(&self) -> DataType {
            DataType::Int64
        }
    }

    struct TestPlugin;

    impl GeneratorPlugin for TestPlugin {
        fn name(&self) -> &str {
            "forty_two"
        }
        fn create(&self, _params: &HashMap<String, String>) -> Box<dyn FieldGenerator> {
            Box::new(FortyTwoGenerator)
        }
    }

    #[test]
    fn register_and_find_plugin() {
        let reg = Registry::new();
        reg.register(Box::new(TestPlugin));

        assert!(reg.find("forty_two").is_some());
        assert!(reg.find("nonexistent").is_none());
    }

    #[test]
    fn registered_names_lists_plugins() {
        let reg = Registry::new();
        reg.register(Box::new(TestPlugin));

        let names = reg.registered_names();
        assert_eq!(names, vec!["forty_two"]);
    }

    #[test]
    fn plugin_creates_working_generator() {
        let reg = Registry::new();
        reg.register(Box::new(TestPlugin));

        let gen = reg.find("forty_two").expect("plugin should exist");
        assert_eq!(gen.output_type(), DataType::Int64);
    }

    #[test]
    fn duplicate_registration_replaces() {
        let reg = Registry::new();
        reg.register(Box::new(TestPlugin));
        reg.register(Box::new(TestPlugin));

        let names = reg.registered_names();
        assert_eq!(names.len(), 1);
    }
}
