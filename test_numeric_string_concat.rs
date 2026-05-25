// Test to verify whether "123" + "456" produces 579 (arithmetic) or "123456" (concat)
#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::collections::HashMap;
    use arrow::array::{ArrayRef, StringArray};
    
    #[test]
    fn numeric_string_concatenation() {
        // This test demonstrates the BREAKING CHANGE
        // Before: "123" + "456" = "123456" (concat)
        // After: "123" + "456" = 579 (arithmetic, then converted to string)
        
        use knit::gen::expr::eval;
        
        let mut cols = HashMap::new();
        cols.insert(
            "a".into(),
            Arc::new(StringArray::from(vec!["123", "100"])) as ArrayRef,
        );
        cols.insert(
            "b".into(),
            Arc::new(StringArray::from(vec!["456", "200"])) as ArrayRef,
        );
        
        // With the new code, this will try to parse "123" and "456" as f64 first
        // and do arithmetic instead of concatenation
        let result = eval::eval_expr("${a} + ${b}", cols);
        
        // What does result contain?
        // If it's string: should be "123456" (old behavior)
        // If it's float64: should be 579.0 (new behavior - BREAKING CHANGE)
    }
}
