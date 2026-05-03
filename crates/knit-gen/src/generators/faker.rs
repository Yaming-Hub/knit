//! Faker-style realistic data generator.
//!
//! Produces human-readable synthetic values — names, emails, addresses, phone
//! numbers, and more — by randomly sampling from embedded word lists. No
//! external faker crate is required; all data is self-contained.
//!
//! Determinism is guaranteed for a given [`RngCore`] state so that the same
//! seed reproduces identical datasets across runs.

use std::sync::Arc;

use arrow::array::{ArrayRef, StringArray};
use arrow::datatypes::DataType;
use rand::RngCore;

use crate::context::GenContext;
use crate::traits::FieldGenerator;

// ---------------------------------------------------------------------------
// Word lists
// ---------------------------------------------------------------------------

/// Common first names (~150 entries).
static FIRST_NAMES: &[&str] = &[
    "James", "Mary", "Robert", "Patricia", "John", "Jennifer", "Michael", "Linda",
    "David", "Elizabeth", "William", "Barbara", "Richard", "Susan", "Joseph", "Jessica",
    "Thomas", "Sarah", "Charles", "Karen", "Christopher", "Lisa", "Daniel", "Nancy",
    "Matthew", "Betty", "Anthony", "Margaret", "Mark", "Sandra", "Donald", "Ashley",
    "Steven", "Kimberly", "Paul", "Emily", "Andrew", "Donna", "Joshua", "Michelle",
    "Kenneth", "Carol", "Kevin", "Amanda", "Brian", "Dorothy", "George", "Melissa",
    "Timothy", "Deborah", "Ronald", "Stephanie", "Edward", "Rebecca", "Jason", "Sharon",
    "Jeffrey", "Laura", "Ryan", "Cynthia", "Jacob", "Kathleen", "Gary", "Amy",
    "Nicholas", "Angela", "Eric", "Shirley", "Jonathan", "Anna", "Stephen", "Brenda",
    "Larry", "Pamela", "Justin", "Emma", "Scott", "Nicole", "Brandon", "Helen",
    "Benjamin", "Samantha", "Samuel", "Katherine", "Raymond", "Christine", "Gregory", "Debra",
    "Frank", "Rachel", "Alexander", "Carolyn", "Patrick", "Janet", "Jack", "Catherine",
    "Dennis", "Maria", "Jerry", "Heather", "Tyler", "Diane", "Aaron", "Ruth",
    "Jose", "Julie", "Adam", "Olivia", "Nathan", "Joyce", "Henry", "Virginia",
    "Peter", "Victoria", "Zachary", "Kelly", "Douglas", "Lauren", "Harold", "Christina",
    "Carl", "Joan", "Arthur", "Evelyn", "Gerald", "Judith", "Roger", "Megan",
    "Keith", "Andrea", "Albert", "Cheryl", "Jeremy", "Hannah", "Terry", "Jacqueline",
    "Sean", "Martha", "Austin", "Gloria", "Randy", "Teresa", "Howard", "Ann",
    "Eugene", "Sara", "Russell", "Madison", "Louis", "Frances", "Philip", "Kathryn",
    "Alice", "Carlos", "Wei", "Yuki", "Omar", "Fatima", "Ravi", "Priya",
];

/// Common last names (~150 entries).
static LAST_NAMES: &[&str] = &[
    "Smith", "Johnson", "Williams", "Brown", "Jones", "Garcia", "Miller", "Davis",
    "Rodriguez", "Martinez", "Hernandez", "Lopez", "Gonzalez", "Wilson", "Anderson", "Thomas",
    "Taylor", "Moore", "Jackson", "Martin", "Lee", "Perez", "Thompson", "White",
    "Harris", "Sanchez", "Clark", "Ramirez", "Lewis", "Robinson", "Walker", "Young",
    "Allen", "King", "Wright", "Scott", "Torres", "Nguyen", "Hill", "Flores",
    "Green", "Adams", "Nelson", "Baker", "Hall", "Rivera", "Campbell", "Mitchell",
    "Carter", "Roberts", "Gomez", "Phillips", "Evans", "Turner", "Diaz", "Parker",
    "Cruz", "Edwards", "Collins", "Reyes", "Stewart", "Morris", "Morales", "Murphy",
    "Cook", "Rogers", "Gutierrez", "Ortiz", "Morgan", "Cooper", "Peterson", "Bailey",
    "Reed", "Kelly", "Howard", "Ramos", "Kim", "Cox", "Ward", "Richardson",
    "Watson", "Brooks", "Chavez", "Wood", "James", "Bennett", "Gray", "Mendoza",
    "Ruiz", "Hughes", "Price", "Alvarez", "Castillo", "Sanders", "Patel", "Myers",
    "Long", "Ross", "Foster", "Jimenez", "Powell", "Jenkins", "Perry", "Russell",
    "Sullivan", "Bell", "Coleman", "Butler", "Henderson", "Barnes", "Gonzales", "Fisher",
    "Vasquez", "Simmons", "Griffin", "Marshall", "Owens", "Harrison", "Fernandez", "McDonald",
    "Woods", "Washington", "Kennedy", "Wells", "Vargas", "Henry", "Chen", "Freeman",
    "Webb", "Tucker", "Burns", "Crawford", "Olson", "Simpson", "Porter", "Hunter",
    "Gordon", "Mendez", "Silva", "Shaw", "Snyder", "Mason", "Dixon", "Munoz",
    "Hunt", "Hicks", "Holmes", "Palmer", "Wagner", "Black", "Robertson", "Boyd",
];

/// Common English words for `word` / `sentence` generation (~200 entries).
static WORDS: &[&str] = &[
    "time", "year", "people", "way", "day", "man", "woman", "child",
    "world", "life", "hand", "part", "place", "case", "week", "company",
    "system", "program", "question", "work", "government", "number", "night", "point",
    "home", "water", "room", "mother", "area", "money", "story", "fact",
    "month", "lot", "right", "study", "book", "eye", "job", "word",
    "business", "issue", "side", "kind", "head", "house", "service", "friend",
    "father", "power", "hour", "game", "line", "end", "member", "law",
    "car", "city", "community", "name", "president", "team", "minute", "idea",
    "body", "information", "back", "parent", "face", "others", "level", "office",
    "door", "health", "person", "art", "war", "history", "party", "result",
    "change", "morning", "reason", "research", "girl", "guy", "moment", "air",
    "teacher", "force", "education", "great", "new", "good", "old", "small",
    "long", "large", "high", "different", "little", "local", "social", "important",
    "national", "young", "possible", "public", "real", "big", "early", "able",
    "political", "major", "special", "human", "certain", "sure", "true", "free",
    "strong", "open", "available", "likely", "clear", "simple", "recent", "common",
    "economic", "current", "similar", "natural", "physical", "dark", "hard", "single",
    "whole", "happy", "serious", "ready", "full", "short", "better", "best",
    "fast", "heavy", "main", "final", "general", "light", "deep", "past",
    "close", "private", "poor", "easy", "direct", "bright", "foreign", "quiet",
    "rich", "modern", "global", "safe", "warm", "cold", "thin", "wide",
    "sharp", "smooth", "soft", "sweet", "wild", "young", "basic", "visual",
    "active", "complex", "critical", "digital", "narrow", "normal", "rare", "regular",
    "secure", "standard", "central", "creative", "primary", "proper", "unique", "useful",
    "vast", "vital", "broad", "exact", "fair", "firm", "formal", "grand",
];

/// Common city names (~150 entries).
static CITIES: &[&str] = &[
    "New York", "Los Angeles", "Chicago", "Houston", "Phoenix", "Philadelphia",
    "San Antonio", "San Diego", "Dallas", "San Jose", "Austin", "Jacksonville",
    "Fort Worth", "Columbus", "Charlotte", "Indianapolis", "San Francisco", "Seattle",
    "Denver", "Nashville", "Oklahoma City", "El Paso", "Washington", "Boston",
    "Las Vegas", "Portland", "Memphis", "Louisville", "Baltimore", "Milwaukee",
    "Albuquerque", "Tucson", "Fresno", "Mesa", "Sacramento", "Atlanta",
    "Kansas City", "Colorado Springs", "Omaha", "Raleigh", "Miami", "Minneapolis",
    "Tampa", "New Orleans", "Arlington", "Cleveland", "Bakersfield", "Aurora",
    "Anaheim", "Honolulu", "Santa Ana", "Riverside", "Corpus Christi", "Lexington",
    "Pittsburgh", "Anchorage", "Stockton", "Cincinnati", "Saint Paul", "Toledo",
    "Newark", "Greensboro", "Buffalo", "Plano", "Lincoln", "Henderson",
    "Fort Wayne", "Jersey City", "Chandler", "Norfolk", "Durham", "Madison",
    "Lubbock", "Irvine", "Winston-Salem", "Glendale", "Garland", "Hialeah",
    "Laredo", "Boise", "Richmond", "Spokane", "Baton Rouge", "Des Moines",
    "Birmingham", "Modesto", "Rochester", "Tacoma", "Fontana", "Oxnard",
    "Moreno Valley", "Fayetteville", "Huntington Beach", "Salt Lake City", "Grand Rapids", "Tallahassee",
    "Worcester", "Knoxville", "Akron", "Brownsville", "Newport News", "Sioux Falls",
    "Chattanooga", "Providence", "Wichita", "Savannah", "Little Rock", "Dayton",
    "Reno", "Peoria", "Tempe", "Eugene", "Hampton", "Salem",
    "Gilbert", "Surprise", "Joliet", "Naperville", "Bridgeport", "Paterson",
    "Topeka", "Macon", "Lakewood", "Odessa", "Pomona", "Escondido",
    "Sunnyvale", "Pasadena", "Hayward", "Torrance", "Visalia", "Roseville",
    "Thornton", "Sterling Heights", "Carrollton", "Denton", "Midland", "Murfreesboro",
    "West Valley City", "Lewisville", "Waco", "Allen", "Sparks", "Pueblo",
    "London", "Paris", "Tokyo", "Berlin", "Sydney", "Toronto",
];

/// Company name parts for composing realistic company names.
static COMPANY_PREFIXES: &[&str] = &[
    "Acme", "Global", "First", "National", "Pacific", "Atlantic", "Summit", "Pinnacle",
    "Vertex", "Apex", "Prime", "Sterling", "Noble", "Quantum", "Nexus", "Vanguard",
    "Horizon", "Eclipse", "Zenith", "Titan", "Phoenix", "Atlas", "Forge", "Pulse",
    "Spark", "Core", "Nova", "Crest", "Peak", "Bridge", "Shield", "Beacon",
    "Evergreen", "Silver", "Golden", "Crystal", "Iron", "Blue", "Red", "Alpha",
    "Omega", "Delta", "Sigma", "Vector", "Metro", "Urban", "Coastal", "Northern",
    "Southern", "Western", "Eastern", "Central", "United", "Allied", "Pioneer", "Liberty",
    "Heritage", "Legacy", "Keystone", "Granite", "Maple", "Oak", "Cedar", "Sage",
];

/// Company name suffixes.
static COMPANY_SUFFIXES: &[&str] = &[
    "Corp", "Inc", "LLC", "Group", "Solutions", "Technologies", "Systems", "Industries",
    "Enterprises", "Services", "Partners", "Associates", "Holdings", "Dynamics", "Consulting",
    "Labs", "Ventures", "Capital", "Networks", "Digital", "Analytics", "Global", "International",
    "Research", "Financial", "Logistics", "Media", "Health", "Bio", "Soft",
];

/// Email domains.
static DOMAINS: &[&str] = &[
    "example.com", "mail.com", "email.com", "inbox.com", "webmail.com",
    "fastmail.net", "proton.me", "outlook.com", "postal.io", "letters.org",
    "testmail.com", "demo.net", "sample.org", "placeholder.com", "mailbox.io",
    "fakemail.com", "tempmail.net", "quickmail.org", "simplemail.com", "postoffice.net",
];

/// Street name bases.
static STREET_NAMES: &[&str] = &[
    "Main", "Oak", "Pine", "Maple", "Cedar", "Elm", "Park", "Lake",
    "Hill", "Walnut", "Sunset", "Ridge", "River", "Spring", "Valley", "Forest",
    "Meadow", "Brook", "Highland", "Willow", "Cherry", "Birch", "Ash", "Laurel",
    "Rose", "Magnolia", "Chestnut", "Spruce", "Poplar", "Sycamore", "Olive", "Peach",
    "Vine", "Church", "School", "Mill", "Bridge", "Market", "Washington", "Jefferson",
    "Lincoln", "Franklin", "Adams", "Jackson", "Harrison", "Madison", "Monroe", "Grant",
];

/// Street suffixes.
static STREET_SUFFIXES: &[&str] = &[
    "St", "Ave", "Blvd", "Dr", "Ln", "Rd", "Ct", "Pl",
    "Way", "Cir", "Pkwy", "Ter",
];

// ---------------------------------------------------------------------------
// Helper
// ---------------------------------------------------------------------------

/// Pick a random element from a static slice using the given RNG.
fn pick<'a>(rng: &mut dyn RngCore, list: &'a [&str]) -> &'a str {
    list[rng.next_u32() as usize % list.len()]
}

// ---------------------------------------------------------------------------
// Generator
// ---------------------------------------------------------------------------

/// Produce realistic synthetic strings for a given faker *category* (method).
///
/// Supported categories: `first_name`, `last_name`, `full_name`, `username`,
/// `email`, `word`, `sentence`, `phone`, `address`, `city`, `company`.
///
/// Unknown categories emit a `tracing::warn` on first call and produce the
/// category name as a constant string — this keeps pipelines running while
/// flagging the issue in logs.
pub struct FakerGenerator {
    /// The faker method / category (e.g. `"email"`, `"first_name"`).
    category: String,
    /// BCP 47 locale hint (currently unused; reserved for future i18n).
    #[allow(dead_code)]
    locale: String,
}

impl FakerGenerator {
    /// Create a new faker generator for the given *category* and *locale*.
    pub fn new(category: String, locale: String) -> Self {
        Self { category, locale }
    }

    /// Generate a single value for the configured category.
    fn generate_one(&self, rng: &mut dyn RngCore) -> String {
        match self.category.as_str() {
            "first_name" => pick(rng, FIRST_NAMES).to_string(),
            "last_name" => pick(rng, LAST_NAMES).to_string(),
            "full_name" | "name" => {
                let first = pick(rng, FIRST_NAMES);
                let last = pick(rng, LAST_NAMES);
                format!("{first} {last}")
            }
            "username" => {
                let name = pick(rng, FIRST_NAMES).to_lowercase();
                let num = rng.next_u32() % 100;
                format!("{name}_{num:02}")
            }
            "email" => {
                let first = pick(rng, FIRST_NAMES).to_lowercase();
                let last = pick(rng, LAST_NAMES).to_lowercase();
                let domain = pick(rng, DOMAINS);
                format!("{first}.{last}@{domain}")
            }
            "word" => pick(rng, WORDS).to_string(),
            "sentence" => {
                let word_count = 3 + (rng.next_u32() % 6) as usize; // 3..=8
                let mut words: Vec<&str> = (0..word_count).map(|_| pick(rng, WORDS)).collect();
                // Capitalise first word.
                let first = words[0];
                let cap = first[..1].to_uppercase() + &first[1..];
                words[0] = Box::leak(cap.into_boxed_str()); // static lifetime for uniform type
                let mut s = words.join(" ");
                s.push('.');
                s
            }
            "phone" => {
                let a = rng.next_u32() % 1000;
                let b = rng.next_u32() % 10000;
                format!("555-{a:03}-{b:04}")
            }
            "address" => {
                let house = 1 + rng.next_u32() % 9999;
                let street = pick(rng, STREET_NAMES);
                let suffix = pick(rng, STREET_SUFFIXES);
                format!("{house} {street} {suffix}")
            }
            "city" => pick(rng, CITIES).to_string(),
            "company" => {
                let prefix = pick(rng, COMPANY_PREFIXES);
                let suffix = pick(rng, COMPANY_SUFFIXES);
                format!("{prefix} {suffix}")
            }
            unknown => {
                tracing::warn!(
                    category = unknown,
                    "unknown faker category, returning category name as constant"
                );
                unknown.to_string()
            }
        }
    }
}

impl FieldGenerator for FakerGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, _ctx: &GenContext) -> ArrayRef {
        let values: Vec<String> = (0..count).map(|_| self.generate_one(rng)).collect();
        Arc::new(StringArray::from(
            values.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        ))
    }

    fn output_type(&self) -> DataType {
        DataType::Utf8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::Array;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use std::collections::HashMap;

    fn make_ctx() -> GenContext<'static> {
        let map: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(HashMap::new()));
        GenContext {
            batch_columns: map,
            row_offset: 0,
            partition_index: 0,
            partition_count: 1,
            entity_name: "test",
        }
    }

    fn gen(category: &str, count: usize, seed: u64) -> ArrayRef {
        let g = FakerGenerator::new(category.into(), "en_US".into());
        let ctx = make_ctx();
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        g.generate(&mut rng, count, &ctx)
    }

    fn strings(arr: &ArrayRef) -> Vec<String> {
        let sa = arr.as_any().downcast_ref::<StringArray>().unwrap();
        (0..sa.len()).map(|i| sa.value(i).to_string()).collect()
    }

    #[test]
    fn first_name_produces_nonempty_strings() {
        let arr = gen("first_name", 50, 1);
        assert_eq!(arr.len(), 50);
        for v in strings(&arr) {
            assert!(!v.is_empty(), "first_name produced empty string");
        }
    }

    #[test]
    fn last_name_produces_nonempty_strings() {
        let arr = gen("last_name", 50, 2);
        assert_eq!(arr.len(), 50);
        for v in strings(&arr) {
            assert!(!v.is_empty());
        }
    }

    #[test]
    fn full_name_contains_space() {
        let arr = gen("full_name", 50, 3);
        for v in strings(&arr) {
            assert!(v.contains(' '), "full_name missing space: {v}");
        }
    }

    #[test]
    fn username_format() {
        let arr = gen("username", 100, 4);
        for v in strings(&arr) {
            assert!(v.contains('_'), "username missing underscore: {v}");
            assert_eq!(v, v.to_lowercase(), "username not lowercase: {v}");
        }
    }

    #[test]
    fn email_contains_at() {
        let arr = gen("email", 100, 5);
        for v in strings(&arr) {
            assert!(v.contains('@'), "email missing @: {v}");
            assert!(v.contains('.'), "email missing dot: {v}");
        }
    }

    #[test]
    fn phone_format() {
        let arr = gen("phone", 100, 6);
        let re_like = |s: &str| -> bool {
            // Expected: 555-DDD-DDDD
            let parts: Vec<&str> = s.split('-').collect();
            parts.len() == 3
                && parts[0] == "555"
                && parts[1].len() == 3
                && parts[2].len() == 4
                && parts[1].chars().all(|c| c.is_ascii_digit())
                && parts[2].chars().all(|c| c.is_ascii_digit())
        };
        for v in strings(&arr) {
            assert!(re_like(&v), "phone bad format: {v}");
        }
    }

    #[test]
    fn sentence_ends_with_period() {
        let arr = gen("sentence", 50, 7);
        for v in strings(&arr) {
            assert!(v.ends_with('.'), "sentence missing period: {v}");
            assert!(v.len() > 5, "sentence too short: {v}");
            // First character should be uppercase.
            assert!(
                v.chars().next().unwrap().is_uppercase(),
                "sentence not capitalised: {v}"
            );
        }
    }

    #[test]
    fn word_produces_nonempty() {
        let arr = gen("word", 50, 8);
        for v in strings(&arr) {
            assert!(!v.is_empty());
        }
    }

    #[test]
    fn address_has_number_and_street() {
        let arr = gen("address", 50, 9);
        for v in strings(&arr) {
            let parts: Vec<&str> = v.splitn(2, ' ').collect();
            assert!(parts.len() == 2, "address missing parts: {v}");
            assert!(
                parts[0].chars().all(|c| c.is_ascii_digit()),
                "address number wrong: {v}"
            );
        }
    }

    #[test]
    fn city_nonempty() {
        let arr = gen("city", 50, 10);
        for v in strings(&arr) {
            assert!(!v.is_empty());
        }
    }

    #[test]
    fn company_nonempty() {
        let arr = gen("company", 50, 11);
        for v in strings(&arr) {
            assert!(v.contains(' '), "company missing space: {v}");
        }
    }

    #[test]
    fn unknown_method_does_not_panic() {
        let arr = gen("nonexistent_method", 10, 12);
        assert_eq!(arr.len(), 10);
        for v in strings(&arr) {
            assert_eq!(v, "nonexistent_method");
        }
    }

    #[test]
    fn deterministic_with_same_seed() {
        let a = gen("email", 20, 42);
        let b = gen("email", 20, 42);
        let va = strings(&a);
        let vb = strings(&b);
        assert_eq!(va, vb, "same seed must produce same output");
    }

    #[test]
    fn correct_count() {
        for count in [0, 1, 5, 100] {
            let arr = gen("first_name", count, 99);
            assert_eq!(arr.len(), count, "wrong count for {count}");
        }
    }

    #[test]
    fn output_type_is_utf8() {
        let g = FakerGenerator::new("email".into(), "en_US".into());
        assert_eq!(g.output_type(), DataType::Utf8);
    }

    #[test]
    fn name_alias_works() {
        // "name" should behave like "full_name"
        let arr = gen("name", 20, 55);
        for v in strings(&arr) {
            assert!(v.contains(' '), "name alias missing space: {v}");
        }
    }
}
