use knit_schema::*;

fn main() {
    // Test 1: Path traversal
    let child = knit_core::DataModel {
        name: "test".to_string(),
        description: None,
        seed: 42,
        locale: "en_US".to_string(),
        timezone: "UTC".to_string(),
        entities: vec![],
        relationships: vec![],
        noise_profiles: vec![],
        correlations: vec![],
        params: std::collections::BTreeMap::new(),
        schema_version: "1.0".to_string(),
    };
    
    // This should be able to traverse to any path
    let result = resolve_extends(
        std::path::Path::new("/some/schema.toml"),
        &child,
        "../../../etc/passwd"
    );
    println!("Path traversal result: {:?}", result);
    
    // Test 2: Uniform distribution min > max
    use knit_core::*;
    let mut model = knit_core::DataModel {
        name: "test".to_string(),
        description: None,
        seed: 42,
        locale: "en_US".to_string(),
        timezone: "UTC".to_string(),
        entities: vec![],
        relationships: vec![],
        noise_profiles: vec![],
        correlations: vec![],
        params: std::collections::BTreeMap::new(),
        schema_version: "1.0".to_string(),
    };
    
    model.entities.push(Entity {
        name: "test".to_string(),
        description: None,
        count: CountSpec::Fixed(10),
        fields: vec![Field {
            name: "value".to_string(),
            description: None,
            data_type: DataType::Float,
            generator: Some(GeneratorSpec::Distribution {
                spec: DistributionSpec {
                    kind: DistributionKind::Uniform,
                    params: {
                        let mut m = std::collections::BTreeMap::new();
                        m.insert("min".to_string(), 100.0);
                        m.insert("max".to_string(), 10.0);
                        m
                    },
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
        }],
        constraints: vec![],
        topology: None,
    });
    
    let errors = validate(&model);
    println!("Uniform min>max errors: {:?}", errors);
}
