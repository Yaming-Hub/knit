//! Smart naming strategies for expanded dimension values.
//!
//! When scaling up a custom dimension (e.g., from 3 regions to 20), we need
//! to generate new plausible value names. This module detects the pattern of
//! existing values and selects an appropriate naming strategy.

/// Detected naming strategy for generating new dimension values.
#[derive(Debug, Clone, PartialEq)]
pub enum NamingStrategy {
    /// 2-letter uppercase codes (likely country/region codes).
    CountryCode,
    /// 2-3 letter uppercase codes (generic short codes).
    ShortCode,
    /// Capitalized English words (likely category names).
    CapitalizedWord,
    /// Numeric suffix pattern (e.g., "Type_1", "Type_2").
    IndexedSuffix {
        /// The common prefix before the numeric index.
        prefix: String,
    },
    /// Fallback: generic indexed values.
    Generic,
}

/// Infer the naming strategy from existing dimension values.
pub fn detect_strategy(values: &[(String, f64)]) -> NamingStrategy {
    if values.is_empty() {
        return NamingStrategy::Generic;
    }

    let names: Vec<&str> = values.iter().map(|(s, _)| s.as_str()).collect();

    // Check for 2-letter uppercase (country codes)
    if names
        .iter()
        .all(|n| n.len() == 2 && n.chars().all(|c| c.is_ascii_uppercase()))
    {
        return NamingStrategy::CountryCode;
    }

    // Check for 2-3 letter uppercase codes
    if names
        .iter()
        .all(|n| (2..=3).contains(&n.len()) && n.chars().all(|c| c.is_ascii_uppercase()))
    {
        return NamingStrategy::ShortCode;
    }

    // Check for mixed-length uppercase codes (2-5 chars, all uppercase)
    // Common for region acronyms like US, EU, APAC, EMEA
    if names
        .iter()
        .all(|n| (2..=5).contains(&n.len()) && n.chars().all(|c| c.is_ascii_uppercase()))
    {
        return NamingStrategy::ShortCode;
    }

    // Check for capitalized English words (single word, first letter upper, rest lower)
    if names.iter().all(|n| is_capitalized_word(n)) {
        return NamingStrategy::CapitalizedWord;
    }

    // Check for indexed suffix pattern (e.g., "Type_1", "Region_2")
    if let Some(prefix) = detect_indexed_prefix(&names) {
        return NamingStrategy::IndexedSuffix { prefix };
    }

    NamingStrategy::Generic
}

/// Generate `count` new values using the given strategy, avoiding `existing` names.
pub fn generate_values(
    strategy: &NamingStrategy,
    existing: &[(String, f64)],
    count: usize,
) -> Vec<String> {
    let existing_set: std::collections::HashSet<&str> =
        existing.iter().map(|(s, _)| s.as_str()).collect();

    match strategy {
        NamingStrategy::CountryCode => pick_unused(COUNTRY_CODES, &existing_set, count),
        NamingStrategy::ShortCode => pick_unused(SHORT_CODES, &existing_set, count),
        NamingStrategy::CapitalizedWord => pick_unused(CATEGORY_WORDS, &existing_set, count),
        NamingStrategy::IndexedSuffix { prefix } => generate_indexed(prefix, &existing_set, count),
        NamingStrategy::Generic => generate_indexed("value", &existing_set, count),
    }
}

fn is_capitalized_word(s: &str) -> bool {
    if s.len() < 3 {
        return false; // Too short to be a word
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_uppercase() {
        return false;
    }
    // Must have at least one lowercase letter (not ALL CAPS)
    let rest: String = chars.collect();
    if rest.chars().all(|c| c.is_ascii_uppercase()) {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_alphabetic() || c == ' ' || c == '-' || c == '\'')
}

fn detect_indexed_prefix(names: &[&str]) -> Option<String> {
    // Look for pattern like "Prefix_N" or "Prefix N" or "PrefixN"
    let mut common_prefix = None;
    for name in names {
        if let Some(idx) = name.rfind('_') {
            let prefix = &name[..idx];
            let suffix = &name[idx + 1..];
            if suffix.chars().all(|c| c.is_ascii_digit()) {
                match &common_prefix {
                    None => common_prefix = Some(prefix.to_string()),
                    Some(p) if p == prefix => {}
                    Some(_) => return None, // inconsistent prefixes
                }
            } else {
                return None;
            }
        } else {
            return None;
        }
    }
    common_prefix
}

fn pick_unused(
    pool: &[&str],
    existing: &std::collections::HashSet<&str>,
    count: usize,
) -> Vec<String> {
    let mut result: Vec<String> = pool
        .iter()
        .filter(|&&v| !existing.contains(v))
        .take(count)
        .map(|&s| s.to_string())
        .collect();

    // If pool exhausted, fall back to indexed
    if result.len() < count {
        let remaining = count - result.len();
        let mut all_used: std::collections::HashSet<String> =
            existing.iter().copied().map(|s| s.to_string()).collect();
        for r in &result {
            all_used.insert(r.clone());
        }
        for i in 1..=(remaining + 100) {
            let name = format!("extra_{}", i);
            if !all_used.contains(&name) {
                all_used.insert(name.clone());
                result.push(name);
                if result.len() >= count {
                    break;
                }
            }
        }
    }

    result
}

fn generate_indexed(
    prefix: &str,
    existing: &std::collections::HashSet<&str>,
    count: usize,
) -> Vec<String> {
    // Find the max existing numeric suffix to continue from
    let max_existing = existing
        .iter()
        .filter_map(|s| {
            s.strip_prefix(prefix)
                .and_then(|rest| rest.strip_prefix('_'))
                .and_then(|num| num.parse::<u64>().ok())
        })
        .max()
        .unwrap_or(0);

    let mut result = Vec::with_capacity(count);
    let mut i = max_existing + 1;
    while result.len() < count {
        let name = format!("{}_{}", prefix, i);
        if !existing.contains(name.as_str()) {
            result.push(name);
        }
        i += 1;
        if i > max_existing + count as u64 + 1000 {
            break; // safety limit
        }
    }
    result
}

// ── Built-in word pools ─────────────────────────────────────────────

/// ISO 3166-1 alpha-2 country codes (most common ones first).
const COUNTRY_CODES: &[&str] = &[
    "US", "GB", "DE", "FR", "JP", "CN", "IN", "BR", "CA", "AU", "IT", "ES", "KR", "MX", "RU", "NL",
    "SE", "CH", "NO", "DK", "FI", "PL", "AT", "BE", "PT", "IE", "NZ", "SG", "HK", "TW", "IL", "ZA",
    "AR", "CL", "CO", "PE", "EG", "NG", "KE", "TH", "MY", "PH", "ID", "VN", "TR", "SA", "AE", "QA",
    "CZ", "RO", "HU", "GR", "UA", "BG", "HR", "SK", "LT", "LV", "EE", "IS",
];

/// Short 2-3 letter codes (generic).
const SHORT_CODES: &[&str] = &[
    "AA", "AB", "AC", "AD", "AE", "AF", "AG", "AH", "AI", "AJ", "BA", "BB", "BC", "BD", "BE", "BF",
    "BG", "BH", "BI", "BJ", "CA", "CB", "CC", "CD", "CE", "CF", "CG", "CH", "CI", "CJ", "DA", "DB",
    "DC", "DD", "DE", "DF", "DG", "DH", "DI", "DJ", "EA", "EB", "EC", "ED", "EE", "EF", "EG", "EH",
    "EI", "EJ", "FA", "FB", "FC", "FD", "FE", "FF", "FG", "FH", "FI", "FJ",
];

/// Plausible category words (mixed domains: products, regions, departments, etc.).
const CATEGORY_WORDS: &[&str] = &[
    // Products / categories
    "Electronics",
    "Clothing",
    "Furniture",
    "Automotive",
    "Groceries",
    "Sporting",
    "Healthcare",
    "Beauty",
    "Toys",
    "Books",
    "Music",
    "Garden",
    "Kitchen",
    "Office",
    "Pets",
    "Travel",
    "Finance",
    "Education",
    "Entertainment",
    "Technology",
    // Regions / locations
    "Northern",
    "Southern",
    "Eastern",
    "Western",
    "Central",
    "Pacific",
    "Atlantic",
    "Mountain",
    "Coastal",
    "Highland",
    "Metro",
    "Suburban",
    "Rural",
    "Downtown",
    "Uptown",
    // Departments / teams
    "Engineering",
    "Marketing",
    "Sales",
    "Support",
    "Operations",
    "Research",
    "Design",
    "Legal",
    "Logistics",
    "Quality",
    // Status / tiers
    "Premium",
    "Standard",
    "Basic",
    "Enterprise",
    "Starter",
    "Advanced",
    "Professional",
    "Ultimate",
    "Essential",
    "Custom",
    // Colors (as category names)
    "Azure",
    "Crimson",
    "Emerald",
    "Golden",
    "Silver",
    "Sapphire",
    "Amber",
    "Ivory",
    "Coral",
    "Slate",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_country_codes() {
        let values = vec![
            ("US".to_string(), 0.5),
            ("GB".to_string(), 0.3),
            ("DE".to_string(), 0.2),
        ];
        assert_eq!(detect_strategy(&values), NamingStrategy::CountryCode);
    }

    #[test]
    fn test_detect_short_codes() {
        let values = vec![
            ("NYC".to_string(), 0.4),
            ("LAX".to_string(), 0.3),
            ("SFO".to_string(), 0.3),
        ];
        assert_eq!(detect_strategy(&values), NamingStrategy::ShortCode);
    }

    #[test]
    fn test_detect_capitalized_words() {
        let values = vec![
            ("Electronics".to_string(), 0.5),
            ("Clothing".to_string(), 0.3),
            ("Furniture".to_string(), 0.2),
        ];
        assert_eq!(detect_strategy(&values), NamingStrategy::CapitalizedWord);
    }

    #[test]
    fn test_detect_indexed_suffix() {
        let values = vec![
            ("Region_1".to_string(), 0.5),
            ("Region_2".to_string(), 0.3),
            ("Region_3".to_string(), 0.2),
        ];
        assert_eq!(
            detect_strategy(&values),
            NamingStrategy::IndexedSuffix {
                prefix: "Region".to_string()
            }
        );
    }

    #[test]
    fn test_generate_country_codes_avoids_existing() {
        let existing = vec![
            ("US".to_string(), 0.5),
            ("GB".to_string(), 0.3),
            ("DE".to_string(), 0.2),
        ];
        let new = generate_values(&NamingStrategy::CountryCode, &existing, 5);
        assert_eq!(new.len(), 5);
        // Should not contain existing values
        assert!(!new.contains(&"US".to_string()));
        assert!(!new.contains(&"GB".to_string()));
        assert!(!new.contains(&"DE".to_string()));
        // Should be valid 2-letter codes
        for code in &new {
            assert_eq!(code.len(), 2);
            assert!(code.chars().all(|c| c.is_ascii_uppercase()));
        }
    }

    #[test]
    fn test_generate_capitalized_words() {
        let existing = vec![
            ("Electronics".to_string(), 0.5),
            ("Clothing".to_string(), 0.5),
        ];
        let new = generate_values(&NamingStrategy::CapitalizedWord, &existing, 3);
        assert_eq!(new.len(), 3);
        assert!(!new.contains(&"Electronics".to_string()));
        assert!(!new.contains(&"Clothing".to_string()));
    }

    #[test]
    fn test_generate_indexed_avoids_existing() {
        let existing = vec![("Type_1".to_string(), 0.5), ("Type_2".to_string(), 0.5)];
        let strategy = NamingStrategy::IndexedSuffix {
            prefix: "Type".to_string(),
        };
        let new = generate_values(&strategy, &existing, 3);
        assert_eq!(new.len(), 3);
        assert!(!new.contains(&"Type_1".to_string()));
        assert!(!new.contains(&"Type_2".to_string()));
        assert!(new.contains(&"Type_3".to_string()));
        assert!(new.contains(&"Type_4".to_string()));
        assert!(new.contains(&"Type_5".to_string()));
    }

    #[test]
    fn test_mixed_case_not_country_code() {
        let values = vec![("Active".to_string(), 0.7), ("Inactive".to_string(), 0.3)];
        // Should be CapitalizedWord, not CountryCode
        assert_eq!(detect_strategy(&values), NamingStrategy::CapitalizedWord);
    }

    #[test]
    fn test_empty_values() {
        let values: Vec<(String, f64)> = vec![];
        assert_eq!(detect_strategy(&values), NamingStrategy::Generic);
    }
}
