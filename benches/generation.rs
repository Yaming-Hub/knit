use std::collections::HashMap;
use std::sync::Arc;

use arrow::record_batch::RecordBatch;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use knit::blueprint::{parse_toml, validate};
use knit::gen::{ActorPool, GenerationEngine};
use knit::plan::compile;

const NUMERIC_BLUEPRINT: &str = r#"
blueprint_version = "1.0"

[model]
name = "numeric_throughput"
seed = 4242

[[entities]]
name = "measurements"
count = 100000

[[entities.fields]]
name = "value"
data_type = "float"
[entities.fields.generator]
type = "distribution"
kind = "normal"
[entities.fields.generator.params]
mean = 100.0
std_dev = 15.0
"#;

const STRING_BLUEPRINT: &str = r#"
blueprint_version = "1.0"

[model]
name = "string_throughput"
seed = 5252

[[entities]]
name = "items"
count = 100000

[[entities.fields]]
name = "sku"
data_type = "string"
[entities.fields.generator]
type = "pattern"
pattern = "ITEM-AAA-######"
"#;

const FK_BLUEPRINT: &str = r#"
blueprint_version = "1.0"

[model]
name = "fk_throughput"
seed = 6262

[[entities]]
name = "parents"
count = 10000

[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1
step = 1

[[entities]]
name = "children"
count = 100000

[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1
step = 1

[[entities.fields]]
name = "parent_id"
data_type = "int"

[[entities.fields]]
name = "amount"
data_type = "float"
[entities.fields.generator]
type = "distribution"
kind = "uniform"
[entities.fields.generator.params]
min = 1.0
max = 1000.0

[[relationships]]
name = "child_parent"
from = "children"
to = "parents"
kind = "many_to_one"
foreign_key = "parent_id"
"#;

const EXPRESSION_BLUEPRINT: &str = r#"
blueprint_version = "1.0"

[model]
name = "expression_throughput"
seed = 7272

[[entities]]
name = "line_items"
count = 50000

[[entities.fields]]
name = "quantity"
data_type = "int"
[entities.fields.generator]
type = "distribution"
kind = "uniform"
[entities.fields.generator.params]
min = 1.0
max = 10.0

[[entities.fields]]
name = "unit_price"
data_type = "float"
[entities.fields.generator]
type = "distribution"
kind = "uniform"
[entities.fields.generator.params]
min = 5.0
max = 250.0

[[entities.fields]]
name = "tax_rate"
data_type = "float"
[entities.fields.generator]
type = "distribution"
kind = "uniform"
[entities.fields.generator.params]
min = 0.05
max = 0.20

[[entities.fields]]
name = "total"
data_type = "float"
[entities.fields.generator]
type = "derived"
expr = "${quantity} * ${unit_price} * (1.0 + ${tax_rate}) |> round(2)"
"#;

const MULTI_ENTITY_BLUEPRINT: &str = r#"
blueprint_version = "1.0"

[model]
name = "multi_entity_pipeline"
seed = 8282

[[entities]]
name = "users"
count = 1000

[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1
step = 1

[[entities.fields]]
name = "email"
data_type = "string"
[entities.fields.generator]
type = "pattern"
pattern = "user####@example.com"

[[entities.fields]]
name = "tier"
data_type = "string"
[entities.fields.generator]
type = "one_of"
choices = [
    { value = "free", weight = 0.6 },
    { value = "pro", weight = 0.3 },
    { value = "enterprise", weight = 0.1 },
]

[[entities]]
name = "products"
count = 500

[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1
step = 1

[[entities.fields]]
name = "sku"
data_type = "string"
[entities.fields.generator]
type = "pattern"
pattern = "SKU-AAA-####"

[[entities.fields]]
name = "price"
data_type = "float"
[entities.fields.generator]
type = "distribution"
kind = "uniform"
[entities.fields.generator.params]
min = 5.0
max = 500.0

[[entities]]
name = "orders"
count = 10000

[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1
step = 1

[[entities.fields]]
name = "user_id"
data_type = "int"

[[entities.fields]]
name = "product_id"
data_type = "int"

[[entities.fields]]
name = "quantity"
data_type = "int"
[entities.fields.generator]
type = "distribution"
kind = "uniform"
[entities.fields.generator.params]
min = 1.0
max = 5.0

[[entities.fields]]
name = "status"
data_type = "string"
[entities.fields.generator]
type = "one_of"
choices = [
    { value = "pending", weight = 0.2 },
    { value = "shipped", weight = 0.3 },
    { value = "delivered", weight = 0.45 },
    { value = "returned", weight = 0.05 },
]

[[relationships]]
name = "order_user"
from = "orders"
to = "users"
kind = "many_to_one"
foreign_key = "user_id"

[[relationships]]
name = "order_product"
from = "orders"
to = "products"
kind = "many_to_one"
foreign_key = "product_id"
"#;

fn generate_from_toml(toml_input: &str) -> HashMap<String, Vec<RecordBatch>> {
    let model = parse_toml(toml_input).expect("parse failed");
    let errors = validate(&model);
    assert!(errors.is_empty(), "validation errors: {errors:?}");
    let plan = compile(&model).expect("compile failed");

    let mut batches: HashMap<String, Vec<RecordBatch>> = HashMap::new();
    let mut engine = GenerationEngine::new();

    if !plan.actor_pool.pools.is_empty() {
        let actor_pool = ActorPool::from_plan(&plan.actor_pool, model.seed);
        engine = engine.with_actor_pool(Arc::new(actor_pool));
        engine.build_graphs(&plan);
    }

    engine
        .execute(&plan, |entity, batch| {
            batches.entry(entity.to_string()).or_default().push(batch);
            Ok(())
        })
        .expect("generation failed");

    batches
}

fn benchmark_pipeline(c: &mut Criterion, name: &str, blueprint: &str) {
    c.bench_function(name, |b| {
        b.iter(|| {
            let batches = generate_from_toml(blueprint);
            black_box(batches);
        });
    });
}

fn numeric_generation_throughput(c: &mut Criterion) {
    benchmark_pipeline(c, "numeric_generation_throughput", NUMERIC_BLUEPRINT);
}

fn string_generation_throughput(c: &mut Criterion) {
    benchmark_pipeline(c, "string_generation_throughput", STRING_BLUEPRINT);
}

fn fk_resolution_throughput(c: &mut Criterion) {
    benchmark_pipeline(c, "fk_resolution_throughput", FK_BLUEPRINT);
}

fn expression_evaluation_throughput(c: &mut Criterion) {
    benchmark_pipeline(c, "expression_evaluation_throughput", EXPRESSION_BLUEPRINT);
}

fn multi_entity_pipeline(c: &mut Criterion) {
    benchmark_pipeline(c, "multi_entity_pipeline", MULTI_ENTITY_BLUEPRINT);
}

criterion_group!(
    benches,
    numeric_generation_throughput,
    string_generation_throughput,
    fk_resolution_throughput,
    expression_evaluation_throughput,
    multi_entity_pipeline,
);
criterion_main!(benches);
