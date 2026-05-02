//! `knit init` — interactive project initialization wizard.
//!
//! Guides the user through template selection and basic configuration,
//! then writes a `.weave.toml` starter schema file.

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::Path;

use anyhow::{bail, Result};
use colored::Colorize;

/// Available project templates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Template {
    /// Users, products, orders, order_items.
    ECommerce,
    /// Devices, sensors, readings, alerts.
    IoT,
    /// Hosts, services, log_entries, errors.
    Logs,
    /// Accounts, transactions, balances.
    Financial,
    /// Empty schema — user fills in everything.
    Custom,
}

impl Template {
    /// All available templates.
    pub const ALL: &'static [Template] = &[
        Template::ECommerce,
        Template::IoT,
        Template::Logs,
        Template::Financial,
        Template::Custom,
    ];

    /// Human-readable name for display.
    pub fn label(self) -> &'static str {
        match self {
            Template::ECommerce => "e-commerce (users, products, orders, order_items)",
            Template::IoT => "IoT (devices, sensors, readings, alerts)",
            Template::Logs => "logs (hosts, services, log_entries, errors)",
            Template::Financial => "financial (accounts, transactions, balances)",
            Template::Custom => "custom (empty schema)",
        }
    }
}

/// Run the init wizard.
///
/// Prompts the user to select a template, configure entity counts, and
/// writes the resulting `.weave.toml` file.
pub fn run(output_path: Option<&str>) -> Result<()> {
    let dest = output_path.unwrap_or(".weave.toml");

    if Path::new(dest).exists() {
        bail!(
            "{} already exists. Remove it first or choose a different path.",
            dest
        );
    }

    println!("{}", "knit init — project setup wizard".bold());
    println!();

    // Template selection
    println!("Select a template:");
    for (i, tpl) in Template::ALL.iter().enumerate() {
        println!("  {} {}", format!("[{}]", i + 1).cyan(), tpl.label());
    }
    let choice = prompt_number("Enter choice (1-5)", 1, 5)?;
    let template = Template::ALL[choice - 1];

    // Entity count
    let row_count = prompt_number("Rows per entity (default 1000)", 1, 10_000_000)?;

    // Seed
    let seed = prompt_number("Random seed (default 42)", 0, u64::MAX as usize)?;

    // Generate the schema
    let schema = generate_template(template, row_count as u64, seed as u64);

    fs::write(dest, &schema)?;
    println!();
    println!(
        "{} wrote {} ({} template)",
        "✓".green().bold(),
        dest.cyan(),
        match template {
            Template::ECommerce => "e-commerce",
            Template::IoT => "IoT",
            Template::Logs => "logs",
            Template::Financial => "financial",
            Template::Custom => "custom",
        }
    );
    println!(
        "  Next: {} to verify, {} to see the plan",
        "knit validate".yellow(),
        "knit plan".yellow()
    );

    Ok(())
}

/// Run init in non-interactive mode with the given template name.
///
/// Used when stdin is not a terminal or for testing.
pub fn run_non_interactive(
    template_name: &str,
    count: u64,
    seed: u64,
    output_path: &str,
) -> Result<()> {
    let template = match template_name {
        "e-commerce" | "ecommerce" => Template::ECommerce,
        "iot" => Template::IoT,
        "logs" => Template::Logs,
        "financial" => Template::Financial,
        "custom" => Template::Custom,
        other => bail!("unknown template: `{}`", other),
    };

    if Path::new(output_path).exists() {
        bail!("{} already exists", output_path);
    }

    let schema = generate_template(template, count, seed);
    fs::write(output_path, &schema)?;
    println!(
        "{} wrote {}",
        "✓".green().bold(),
        output_path.cyan()
    );
    Ok(())
}

/// Prompt for a number within a range, reading from stdin.
fn prompt_number(prompt: &str, min: usize, max: usize) -> Result<usize> {
    print!("{}: ", prompt);
    io::stdout().flush()?;

    let stdin = io::stdin();
    let line = stdin.lock().lines().next().unwrap_or(Ok(String::new()))?;
    let trimmed = line.trim();

    if trimmed.is_empty() {
        // Return a sensible default
        return Ok(min.max(1));
    }

    let val: usize = trimmed.parse().map_err(|_| {
        anyhow::anyhow!("invalid number: `{}`", trimmed)
    })?;

    if val < min || val > max {
        bail!("value must be between {} and {}", min, max);
    }
    Ok(val)
}

/// Generate a `.weave.toml` schema string from a template.
pub fn generate_template(template: Template, count: u64, seed: u64) -> String {
    match template {
        Template::ECommerce => template_ecommerce(count, seed),
        Template::IoT => template_iot(count, seed),
        Template::Logs => template_logs(count, seed),
        Template::Financial => template_financial(count, seed),
        Template::Custom => template_custom(count, seed),
    }
}

/// E-commerce template: users, products, orders, order_items.
fn template_ecommerce(count: u64, seed: u64) -> String {
    format!(
        r#"schema_version = "1.0"

[model]
name = "ecommerce"
description = "E-commerce data model with users, products, and orders"
seed = {seed}
locale = "en_US"
timezone = "UTC"

[[entities]]
name = "users"
count = {count}

[[entities.fields]]
name = "id"
data_type = "uuid"
primary_key = true
[entities.fields.generator]
type = "uuid"

[[entities.fields]]
name = "name"
data_type = "string"
[entities.fields.generator]
type = "faker"
category = "name"
locale = "en_US"

[[entities.fields]]
name = "email"
data_type = "string"
[entities.fields.generator]
type = "faker"
category = "email"
locale = "en_US"

[[entities.fields]]
name = "created_at"
data_type = "datetime"
[entities.fields.generator]
type = "temporal"
kind = "datetime"
start = "2020-01-01T00:00:00Z"
end = "2024-12-31T23:59:59Z"

[[entities]]
name = "products"
count = {product_count}

[[entities.fields]]
name = "id"
data_type = "uuid"
primary_key = true
[entities.fields.generator]
type = "uuid"

[[entities.fields]]
name = "name"
data_type = "string"
[entities.fields.generator]
type = "faker"
category = "commerce_product"
locale = "en_US"

[[entities.fields]]
name = "price"
data_type = "float"
[entities.fields.generator]
type = "distribution"
kind = "uniform"
min = 1.0
max = 999.99

[[entities]]
name = "orders"
count = {order_count}

[[entities.fields]]
name = "id"
data_type = "uuid"
primary_key = true
[entities.fields.generator]
type = "uuid"

[[entities.fields]]
name = "user_id"
data_type = "uuid"
[entities.fields.generator]
type = "foreign_key"
target_entity = "users"
target_field = "id"

[[entities.fields]]
name = "ordered_at"
data_type = "datetime"
[entities.fields.generator]
type = "temporal"
kind = "datetime"
start = "2020-01-01T00:00:00Z"
end = "2024-12-31T23:59:59Z"

[[entities]]
name = "order_items"
count = {item_count}

[[entities.fields]]
name = "id"
data_type = "uuid"
primary_key = true
[entities.fields.generator]
type = "uuid"

[[entities.fields]]
name = "order_id"
data_type = "uuid"
[entities.fields.generator]
type = "foreign_key"
target_entity = "orders"
target_field = "id"

[[entities.fields]]
name = "product_id"
data_type = "uuid"
[entities.fields.generator]
type = "foreign_key"
target_entity = "products"
target_field = "id"

[[entities.fields]]
name = "quantity"
data_type = "int"
[entities.fields.generator]
type = "distribution"
kind = "uniform"
min = 1.0
max = 10.0

[[relationships]]
name = "user_orders"
from_entity = "orders"
from_field = "user_id"
to_entity = "users"
to_field = "id"
kind = "many_to_one"

[[relationships]]
name = "order_items_order"
from_entity = "order_items"
from_field = "order_id"
to_entity = "orders"
to_field = "id"
kind = "many_to_one"

[[relationships]]
name = "order_items_product"
from_entity = "order_items"
from_field = "product_id"
to_entity = "products"
to_field = "id"
kind = "many_to_one"
"#,
        seed = seed,
        count = count,
        product_count = count / 10,
        order_count = count * 3,
        item_count = count * 8,
    )
}

/// IoT template: devices, sensors, readings, alerts.
fn template_iot(count: u64, seed: u64) -> String {
    format!(
        r#"schema_version = "1.0"

[model]
name = "iot"
description = "IoT data model with devices, sensors, readings, and alerts"
seed = {seed}
locale = "en_US"
timezone = "UTC"

[[entities]]
name = "devices"
count = {count}

[[entities.fields]]
name = "id"
data_type = "uuid"
primary_key = true
[entities.fields.generator]
type = "uuid"

[[entities.fields]]
name = "name"
data_type = "string"
[entities.fields.generator]
type = "pattern"
pattern = "device-[A-Z]{{3}}-[0-9]{{4}}"

[[entities.fields]]
name = "location"
data_type = "string"
[entities.fields.generator]
type = "one_of"
choices = ["warehouse-A", "warehouse-B", "factory-1", "factory-2", "office"]

[[entities]]
name = "sensors"
count = {sensor_count}

[[entities.fields]]
name = "id"
data_type = "uuid"
primary_key = true
[entities.fields.generator]
type = "uuid"

[[entities.fields]]
name = "device_id"
data_type = "uuid"
[entities.fields.generator]
type = "foreign_key"
target_entity = "devices"
target_field = "id"

[[entities.fields]]
name = "sensor_type"
data_type = "string"
[entities.fields.generator]
type = "one_of"
choices = ["temperature", "humidity", "pressure", "vibration", "voltage"]

[[entities]]
name = "readings"
count = {reading_count}

[[entities.fields]]
name = "id"
data_type = "uuid"
primary_key = true
[entities.fields.generator]
type = "uuid"

[[entities.fields]]
name = "sensor_id"
data_type = "uuid"
[entities.fields.generator]
type = "foreign_key"
target_entity = "sensors"
target_field = "id"

[[entities.fields]]
name = "value"
data_type = "float"
[entities.fields.generator]
type = "distribution"
kind = "normal"
mean = 50.0
std_dev = 15.0

[[entities.fields]]
name = "recorded_at"
data_type = "datetime"
[entities.fields.generator]
type = "temporal"
kind = "datetime"
start = "2024-01-01T00:00:00Z"
end = "2024-12-31T23:59:59Z"

[[entities]]
name = "alerts"
count = {alert_count}

[[entities.fields]]
name = "id"
data_type = "uuid"
primary_key = true
[entities.fields.generator]
type = "uuid"

[[entities.fields]]
name = "sensor_id"
data_type = "uuid"
[entities.fields.generator]
type = "foreign_key"
target_entity = "sensors"
target_field = "id"

[[entities.fields]]
name = "severity"
data_type = "string"
[entities.fields.generator]
type = "one_of"
choices = ["low", "medium", "high", "critical"]

[[relationships]]
name = "sensor_device"
from_entity = "sensors"
from_field = "device_id"
to_entity = "devices"
to_field = "id"
kind = "many_to_one"

[[relationships]]
name = "reading_sensor"
from_entity = "readings"
from_field = "sensor_id"
to_entity = "sensors"
to_field = "id"
kind = "many_to_one"

[[relationships]]
name = "alert_sensor"
from_entity = "alerts"
from_field = "sensor_id"
to_entity = "sensors"
to_field = "id"
kind = "many_to_one"
"#,
        seed = seed,
        count = count,
        sensor_count = count * 3,
        reading_count = count * 100,
        alert_count = count / 5,
    )
}

/// Logs template: hosts, services, log_entries, errors.
fn template_logs(count: u64, seed: u64) -> String {
    format!(
        r#"schema_version = "1.0"

[model]
name = "logs"
description = "Log data model with hosts, services, log entries, and errors"
seed = {seed}
locale = "en_US"
timezone = "UTC"

[[entities]]
name = "hosts"
count = {count}

[[entities.fields]]
name = "id"
data_type = "uuid"
primary_key = true
[entities.fields.generator]
type = "uuid"

[[entities.fields]]
name = "hostname"
data_type = "string"
[entities.fields.generator]
type = "pattern"
pattern = "host-[a-z]{{4}}-[0-9]{{2}}"

[[entities.fields]]
name = "region"
data_type = "string"
[entities.fields.generator]
type = "one_of"
choices = ["us-east-1", "us-west-2", "eu-west-1", "ap-southeast-1"]

[[entities]]
name = "services"
count = {service_count}

[[entities.fields]]
name = "id"
data_type = "uuid"
primary_key = true
[entities.fields.generator]
type = "uuid"

[[entities.fields]]
name = "name"
data_type = "string"
[entities.fields.generator]
type = "one_of"
choices = ["api-gateway", "auth-service", "data-pipeline", "web-frontend", "worker"]

[[entities.fields]]
name = "host_id"
data_type = "uuid"
[entities.fields.generator]
type = "foreign_key"
target_entity = "hosts"
target_field = "id"

[[entities]]
name = "log_entries"
count = {log_count}

[[entities.fields]]
name = "id"
data_type = "uuid"
primary_key = true
[entities.fields.generator]
type = "uuid"

[[entities.fields]]
name = "service_id"
data_type = "uuid"
[entities.fields.generator]
type = "foreign_key"
target_entity = "services"
target_field = "id"

[[entities.fields]]
name = "level"
data_type = "string"
[entities.fields.generator]
type = "one_of"
choices = ["DEBUG", "INFO", "WARN", "ERROR"]
weights = [0.1, 0.6, 0.2, 0.1]

[[entities.fields]]
name = "message"
data_type = "string"
[entities.fields.generator]
type = "faker"
category = "sentence"
locale = "en_US"

[[entities.fields]]
name = "timestamp"
data_type = "datetime"
[entities.fields.generator]
type = "temporal"
kind = "datetime"
start = "2024-01-01T00:00:00Z"
end = "2024-12-31T23:59:59Z"

[[entities]]
name = "errors"
count = {error_count}

[[entities.fields]]
name = "id"
data_type = "uuid"
primary_key = true
[entities.fields.generator]
type = "uuid"

[[entities.fields]]
name = "log_entry_id"
data_type = "uuid"
[entities.fields.generator]
type = "foreign_key"
target_entity = "log_entries"
target_field = "id"

[[entities.fields]]
name = "error_code"
data_type = "string"
[entities.fields.generator]
type = "pattern"
pattern = "E[0-9]{{4}}"

[[entities.fields]]
name = "stack_trace"
data_type = "string"
[entities.fields.generator]
type = "faker"
category = "paragraph"
locale = "en_US"

[[relationships]]
name = "service_host"
from_entity = "services"
from_field = "host_id"
to_entity = "hosts"
to_field = "id"
kind = "many_to_one"

[[relationships]]
name = "log_service"
from_entity = "log_entries"
from_field = "service_id"
to_entity = "services"
to_field = "id"
kind = "many_to_one"

[[relationships]]
name = "error_log"
from_entity = "errors"
from_field = "log_entry_id"
to_entity = "log_entries"
to_field = "id"
kind = "many_to_one"
"#,
        seed = seed,
        count = count,
        service_count = count * 5,
        log_count = count * 500,
        error_count = count * 50,
    )
}

/// Financial template: accounts, transactions, balances.
fn template_financial(count: u64, seed: u64) -> String {
    format!(
        r#"schema_version = "1.0"

[model]
name = "financial"
description = "Financial data model with accounts, transactions, and balances"
seed = {seed}
locale = "en_US"
timezone = "UTC"

[[entities]]
name = "accounts"
count = {count}

[[entities.fields]]
name = "id"
data_type = "uuid"
primary_key = true
[entities.fields.generator]
type = "uuid"

[[entities.fields]]
name = "account_number"
data_type = "string"
[entities.fields.generator]
type = "pattern"
pattern = "ACCT-[0-9]{{8}}"

[[entities.fields]]
name = "account_type"
data_type = "string"
[entities.fields.generator]
type = "one_of"
choices = ["checking", "savings", "credit", "investment"]

[[entities.fields]]
name = "currency"
data_type = "string"
[entities.fields.generator]
type = "one_of"
choices = ["USD", "EUR", "GBP", "JPY"]

[[entities.fields]]
name = "opened_at"
data_type = "datetime"
[entities.fields.generator]
type = "temporal"
kind = "datetime"
start = "2015-01-01T00:00:00Z"
end = "2024-12-31T23:59:59Z"

[[entities]]
name = "transactions"
count = {tx_count}

[[entities.fields]]
name = "id"
data_type = "uuid"
primary_key = true
[entities.fields.generator]
type = "uuid"

[[entities.fields]]
name = "account_id"
data_type = "uuid"
[entities.fields.generator]
type = "foreign_key"
target_entity = "accounts"
target_field = "id"

[[entities.fields]]
name = "amount"
data_type = "float"
[entities.fields.generator]
type = "distribution"
kind = "normal"
mean = 150.0
std_dev = 500.0

[[entities.fields]]
name = "tx_type"
data_type = "string"
[entities.fields.generator]
type = "one_of"
choices = ["deposit", "withdrawal", "transfer", "payment", "refund"]

[[entities.fields]]
name = "timestamp"
data_type = "datetime"
[entities.fields.generator]
type = "temporal"
kind = "datetime"
start = "2024-01-01T00:00:00Z"
end = "2024-12-31T23:59:59Z"

[[entities]]
name = "balances"
count = {balance_count}

[[entities.fields]]
name = "id"
data_type = "uuid"
primary_key = true
[entities.fields.generator]
type = "uuid"

[[entities.fields]]
name = "account_id"
data_type = "uuid"
[entities.fields.generator]
type = "foreign_key"
target_entity = "accounts"
target_field = "id"

[[entities.fields]]
name = "balance"
data_type = "float"
[entities.fields.generator]
type = "distribution"
kind = "normal"
mean = 5000.0
std_dev = 10000.0

[[entities.fields]]
name = "as_of"
data_type = "datetime"
[entities.fields.generator]
type = "temporal"
kind = "datetime"
start = "2024-01-01T00:00:00Z"
end = "2024-12-31T23:59:59Z"

[[relationships]]
name = "tx_account"
from_entity = "transactions"
from_field = "account_id"
to_entity = "accounts"
to_field = "id"
kind = "many_to_one"

[[relationships]]
name = "balance_account"
from_entity = "balances"
from_field = "account_id"
to_entity = "accounts"
to_field = "id"
kind = "many_to_one"
"#,
        seed = seed,
        count = count,
        tx_count = count * 20,
        balance_count = count * 12,
    )
}

/// Custom (empty) template.
fn template_custom(count: u64, seed: u64) -> String {
    format!(
        r#"schema_version = "1.0"

[model]
name = "custom"
description = "Custom schema — add your entities below"
seed = {seed}
locale = "en_US"
timezone = "UTC"

[[entities]]
name = "example"
count = {count}

[[entities.fields]]
name = "id"
data_type = "uuid"
primary_key = true
[entities.fields.generator]
type = "uuid"

[[entities.fields]]
name = "value"
data_type = "string"
[entities.fields.generator]
type = "faker"
category = "word"
locale = "en_US"
"#,
        seed = seed,
        count = count,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ecommerce_template_valid_toml() {
        let schema = generate_template(Template::ECommerce, 1000, 42);
        assert!(schema.contains("name = \"ecommerce\""));
        assert!(schema.contains("[[entities]]"));
        assert!(schema.contains("name = \"users\""));
        assert!(schema.contains("name = \"products\""));
        assert!(schema.contains("name = \"orders\""));
        assert!(schema.contains("name = \"order_items\""));
        // Verify it's valid TOML
        let _: toml::Value = toml::from_str(&schema).expect("template should be valid TOML");
    }

    #[test]
    fn iot_template_valid_toml() {
        let schema = generate_template(Template::IoT, 500, 99);
        assert!(schema.contains("name = \"iot\""));
        assert!(schema.contains("name = \"devices\""));
        assert!(schema.contains("name = \"sensors\""));
        assert!(schema.contains("name = \"readings\""));
        assert!(schema.contains("name = \"alerts\""));
        let _: toml::Value = toml::from_str(&schema).expect("template should be valid TOML");
    }

    #[test]
    fn logs_template_valid_toml() {
        let schema = generate_template(Template::Logs, 100, 1);
        assert!(schema.contains("name = \"logs\""));
        assert!(schema.contains("name = \"hosts\""));
        assert!(schema.contains("name = \"services\""));
        assert!(schema.contains("name = \"log_entries\""));
        assert!(schema.contains("name = \"errors\""));
        let _: toml::Value = toml::from_str(&schema).expect("template should be valid TOML");
    }

    #[test]
    fn financial_template_valid_toml() {
        let schema = generate_template(Template::Financial, 200, 7);
        assert!(schema.contains("name = \"financial\""));
        assert!(schema.contains("name = \"accounts\""));
        assert!(schema.contains("name = \"transactions\""));
        assert!(schema.contains("name = \"balances\""));
        let _: toml::Value = toml::from_str(&schema).expect("template should be valid TOML");
    }

    #[test]
    fn custom_template_valid_toml() {
        let schema = generate_template(Template::Custom, 50, 0);
        assert!(schema.contains("name = \"custom\""));
        let _: toml::Value = toml::from_str(&schema).expect("template should be valid TOML");
    }

    #[test]
    fn template_respects_count_and_seed() {
        let schema = generate_template(Template::Custom, 777, 12345);
        assert!(schema.contains("count = 777"));
        assert!(schema.contains("seed = 12345"));
    }

    #[test]
    fn non_interactive_unknown_template() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.weave.toml");
        let result = run_non_interactive(
            "nope",
            100,
            42,
            path.to_str().unwrap(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn non_interactive_writes_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.weave.toml");
        run_non_interactive(
            "e-commerce",
            500,
            42,
            path.to_str().unwrap(),
        )
        .unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("name = \"ecommerce\""));
    }
}
