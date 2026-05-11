//! Plugin registry for custom generator extensions.
//!
//! This module provides a [`Registry`] that allows external code to register
//! custom [`FieldGenerator`] implementations at runtime. Registered plugins
//! are automatically consulted during generation when a schema field uses
//! `type = "plugin"` with a matching `name`.
//!
//! # Usage
//!
//! ```no_run
//! use std::collections::BTreeMap;
//! use knit::gen::plugin::{registry, GeneratorPlugin};
//! use knit::gen::traits::FieldGenerator;
//!
//! struct MyPlugin;
//! impl GeneratorPlugin for MyPlugin {
//!     fn name(&self) -> &str { "my_custom" }
//!     fn create(
//!         &self,
//!         _params: &BTreeMap<String, knit::core::Value>,
//!     ) -> Result<Box<dyn FieldGenerator>, String> {
//!         unimplemented!("provide your generator here")
//!     }
//! }
//!
//! registry().register(Box::new(MyPlugin));
//! ```

use std::collections::BTreeMap;
use std::sync::{OnceLock, RwLock};

use crate::gen::traits::FieldGenerator;

/// A trait for custom generator plugins that can be registered at runtime.
///
/// Implementors provide a unique name (used in schema `type = "plugin"` fields)
/// and a factory method that produces a [`FieldGenerator`] from typed parameters.
pub trait GeneratorPlugin: Send + Sync {
    /// Unique name for this generator type (used in schema `name = "..."`).
    fn name(&self) -> &str;

    /// Create a [`FieldGenerator`] instance from the given parameters.
    ///
    /// Returns an error string if required parameters are missing or invalid.
    fn create(
        &self,
        params: &BTreeMap<String, crate::core::Value>,
    ) -> Result<Box<dyn FieldGenerator>, String>;
}

/// Thread-safe registry of [`GeneratorPlugin`] instances.
///
/// Plugins are stored in insertion order and looked up by name.
/// The registry is globally accessible via [`registry()`].
pub struct Registry {
    plugins: RwLock<Vec<std::sync::Arc<dyn GeneratorPlugin>>>,
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
        let plugin: std::sync::Arc<dyn GeneratorPlugin> = plugin.into();
        let mut plugins = self.plugins.write().expect("plugin registry lock poisoned");
        let name = plugin.name().to_string();
        plugins.retain(|p| p.name() != name);
        tracing::info!(plugin = %name, "registered generator plugin");
        plugins.push(plugin);
    }

    /// Look up a registered plugin by name and create a generator with default (empty) params.
    ///
    /// Returns `None` if no plugin with the given name has been registered.
    /// Returns `Some(Err(...))` if the plugin exists but creation fails.
    pub fn find(&self, name: &str) -> Option<Result<Box<dyn FieldGenerator>, String>> {
        let plugin = {
            let plugins = self.plugins.read().expect("plugin registry lock poisoned");
            plugins.iter().find(|p| p.name() == name).cloned()
        };
        let params = BTreeMap::new();
        plugin.map(|p| p.create(&params))
    }

    /// Look up a plugin by name and create a generator with the given parameters.
    ///
    /// Returns `None` if no plugin with the given name has been registered.
    /// Returns `Some(Err(...))` if the plugin exists but creation fails.
    pub fn create(
        &self,
        name: &str,
        params: &BTreeMap<String, crate::core::Value>,
    ) -> Option<Result<Box<dyn FieldGenerator>, String>> {
        let plugin = {
            let plugins = self.plugins.read().expect("plugin registry lock poisoned");
            plugins.iter().find(|p| p.name() == name).cloned()
        };
        plugin.map(|p| p.create(params))
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

    use crate::gen::context::GenContext;

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
        fn create(
            &self,
            _params: &BTreeMap<String, crate::core::Value>,
        ) -> Result<Box<dyn FieldGenerator>, String> {
            Ok(Box::new(FortyTwoGenerator))
        }
    }

    struct FailingPlugin;

    impl GeneratorPlugin for FailingPlugin {
        fn name(&self) -> &str {
            "failing"
        }
        fn create(
            &self,
            params: &BTreeMap<String, crate::core::Value>,
        ) -> Result<Box<dyn FieldGenerator>, String> {
            if params.contains_key("required_param") {
                Ok(Box::new(FortyTwoGenerator))
            } else {
                Err("missing required_param".to_string())
            }
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

        let gen = reg.find("forty_two").unwrap().unwrap();
        assert_eq!(gen.output_type(), DataType::Int64);
    }

    #[test]
    fn plugin_creation_can_fail() {
        let reg = Registry::new();
        reg.register(Box::new(FailingPlugin));

        let result = reg.find("failing").unwrap();
        assert!(result.is_err());
        let err = match result {
            Err(e) => e,
            Ok(_) => unreachable!(),
        };
        assert_eq!(err, "missing required_param");
    }

    #[test]
    fn plugin_creation_with_params_succeeds() {
        let reg = Registry::new();
        reg.register(Box::new(FailingPlugin));

        let mut params = BTreeMap::new();
        params.insert(
            "required_param".to_string(),
            crate::core::Value::String("value".to_string()),
        );
        let result = reg.create("failing", &params).unwrap();
        assert!(result.is_ok());
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