use knit::core::*;

fn main() {
    // Test Entity with stats: None
    let entity = Entity {
        name: "test".into(),
        description: None,
        tags: vec![],
        count: CountSpec::Fixed(100),
        fields: vec![Field {
            name: "id".into(),
            description: None,
            data_type: DataType::Int,
            generator: None,
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
            stats: None,
        }],
        constraints: vec![],
        topology: None,
        actor: false,
        persona_distribution: None,
        activity_count: None,
        mixin_refs: None,
        output: None,
        stats: None,
    };
    
    let toml = toml::to_string(&entity).unwrap();
    println!("TOML with stats=None:");
    println!("{}", toml);
    println!("Contains 'stats'? {}", toml.contains("stats"));
    
    // Test Entity with stats: Some
    let mut entity2 = entity.clone();
    entity2.stats = Some(TableStats {
        source_rows: 1000,
        rows_per_partition: None,
    });
    
    let toml2 = toml::to_string(&entity2).unwrap();
    println!("\nTOML with stats=Some:");
    println!("{}", toml2);
    println!("Contains 'stats'? {}", toml2.contains("stats"));
}
