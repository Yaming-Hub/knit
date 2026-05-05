//! Faker-style realistic data generator.
//!
//! Produces human-readable synthetic values — names, emails, addresses, phone
//! numbers, and more — by randomly sampling from embedded word lists. No
//! external faker crate is required; all data is self-contained.
//!
//! Determinism is guaranteed for a given [`RngCore`] state so that the same
//! seed reproduces identical datasets across runs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use arrow::array::{ArrayRef, Date32Array, StringArray, TimestampNanosecondArray};
use arrow::datatypes::{DataType, TimeUnit};
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

/// US state names (~50 entries).
static US_STATES: &[&str] = &[
    "Alabama", "Alaska", "Arizona", "Arkansas", "California", "Colorado", "Connecticut",
    "Delaware", "Florida", "Georgia", "Hawaii", "Idaho", "Illinois", "Indiana", "Iowa",
    "Kansas", "Kentucky", "Louisiana", "Maine", "Maryland", "Massachusetts", "Michigan",
    "Minnesota", "Mississippi", "Missouri", "Montana", "Nebraska", "Nevada", "New Hampshire",
    "New Jersey", "New Mexico", "New York", "North Carolina", "North Dakota", "Ohio",
    "Oklahoma", "Oregon", "Pennsylvania", "Rhode Island", "South Carolina", "South Dakota",
    "Tennessee", "Texas", "Utah", "Vermont", "Virginia", "Washington", "West Virginia",
    "Wisconsin", "Wyoming",
];

/// Country names (~60 entries).
static COUNTRIES: &[&str] = &[
    "United States", "Canada", "United Kingdom", "France", "Germany", "Italy", "Spain",
    "Australia", "Japan", "China", "India", "Brazil", "Mexico", "South Korea", "Russia",
    "Netherlands", "Switzerland", "Sweden", "Norway", "Denmark", "Finland", "Belgium",
    "Austria", "Ireland", "Portugal", "Poland", "Czech Republic", "Greece", "Turkey",
    "Israel", "South Africa", "Egypt", "Nigeria", "Kenya", "Argentina", "Colombia",
    "Chile", "Peru", "Thailand", "Vietnam", "Indonesia", "Philippines", "Malaysia",
    "Singapore", "New Zealand", "Saudi Arabia", "United Arab Emirates", "Pakistan",
    "Bangladesh", "Taiwan", "Hong Kong", "Ukraine", "Romania", "Hungary", "Croatia",
    "Serbia", "Slovakia", "Slovenia", "Iceland", "Luxembourg",
];

/// Color names for `color` method.
static COLORS: &[&str] = &[
    "red", "blue", "green", "yellow", "orange", "purple", "pink", "brown",
    "black", "white", "gray", "cyan", "magenta", "lime", "teal", "navy",
    "maroon", "olive", "aqua", "coral", "crimson", "gold", "indigo", "ivory",
    "khaki", "lavender", "orchid", "plum", "salmon", "sienna", "silver", "tan",
    "turquoise", "violet", "wheat", "beige", "azure", "chartreuse", "fuchsia", "scarlet",
];

/// Top-level domains for URL generation.
static TLDS: &[&str] = &[
    "com", "org", "net", "io", "dev", "co", "app", "tech",
];

/// Email domains.
static DOMAINS: &[&str] = &[
    "example.com", "mail.com", "email.com", "inbox.com", "webmail.com",
    "fastmail.net", "proton.me", "outlook.com", "postal.io", "letters.org",
    "testmail.com", "demo.net", "sample.org", "placeholder.com", "mailbox.io",
    "fakemail.com", "tempmail.net", "quickmail.org", "simplemail.com", "postoffice.net",
];

/// Product adjectives for `product_name` generation.
static PRODUCT_ADJECTIVES: &[&str] = &[
    "Ultra", "Pro", "Classic", "Premium", "Essential", "Advanced", "Compact",
    "Deluxe", "Elite", "Smart", "Eco", "Turbo", "Slim", "Heavy-Duty", "Portable",
    "Wireless", "Digital", "Organic", "Natural", "Industrial", "Precision", "Royal",
    "Mega", "Mini", "Supreme", "Rapid", "Silent", "Flex", "Hyper", "Quantum",
];

/// Product materials/descriptors for `product_name` generation.
static PRODUCT_MATERIALS: &[&str] = &[
    "Steel", "Bamboo", "Cotton", "Granite", "Leather", "Silk", "Rubber", "Bronze",
    "Ceramic", "Carbon", "Titanium", "Copper", "Wooden", "Plastic", "Glass", "Marble",
    "Linen", "Concrete", "Frozen", "Fresh", "Soft", "Recycled", "Chrome", "Velvet",
];

/// Product nouns for `product_name` generation.
static PRODUCT_NOUNS: &[&str] = &[
    "Headphones", "Keyboard", "Chair", "Lamp", "Backpack", "Wallet", "Watch", "Shoes",
    "Blender", "Towels", "Gloves", "Jacket", "Bottle", "Speaker", "Camera", "Tablet",
    "Pan", "Socks", "Hat", "Tuna", "Chips", "Soap", "Cheese", "Salad",
    "Pizza", "Bike", "Ball", "Table", "Shirt", "Pants", "Mouse", "Monitor",
    "Bench", "Pillow", "Candle", "Knife", "Mug", "Clock", "Brush", "Blanket",
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
/// Supported categories: `first_name`, `last_name`, `full_name` / `name`,
/// `username`, `email`, `word`, `sentence`, `paragraph`, `title`, `phone`,
/// `address`, `city`, `state`, `country`, `zip_code` / `zipcode` / `postal_code`,
/// `company`, `product_name` / `product`, `url`, `domain`, `ipv4` / `ip_address`,
/// `ipv6`, `color`, `hex_color`.
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
    /// Optional arguments (e.g. date range as ISO strings).
    args: Vec<knit_core::Value>,
    /// Whether a warning has already been emitted for an unknown category.
    warned: AtomicBool,
}

impl FakerGenerator {
    /// Create a new faker generator for the given *category* and *locale*.
    pub fn new(category: String, locale: String, args: Vec<knit_core::Value>) -> Self {
        Self {
            category,
            locale,
            args,
            warned: AtomicBool::new(false),
        }
    }

    /// Parse date range from args, falling back to 2020-01-01..2024-12-31.
    fn date_range(&self) -> (i64, i64) {
        let default_start = days_from_epoch(2020, 1, 1);
        let default_end = days_from_epoch(2024, 12, 31);

        let parse_date = |v: &knit_core::Value| -> Option<i64> {
            if let knit_core::Value::String(s) = v {
                let parts: Vec<&str> = s.split('-').collect();
                if parts.len() == 3 {
                    let y = parts[0].parse::<i32>().ok()?;
                    let m = parts[1].parse::<u32>().ok()?;
                    let d = parts[2].parse::<u32>().ok()?;
                    return Some(days_from_epoch(y, m, d));
                }
            }
            None
        };

        let start = self.args.first().and_then(parse_date).unwrap_or(default_start);
        let end = self.args.get(1).and_then(parse_date).unwrap_or(default_end);
        (start, end)
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
                let mut s = String::with_capacity(word_count * 7);
                for i in 0..word_count {
                    if i > 0 {
                        s.push(' ');
                    }
                    let w = pick(rng, WORDS);
                    if i == 0 {
                        // Capitalize first character
                        let mut chars = w.chars();
                        if let Some(c) = chars.next() {
                            for uc in c.to_uppercase() {
                                s.push(uc);
                            }
                            s.push_str(chars.as_str());
                        }
                    } else {
                        s.push_str(w);
                    }
                }
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
            "product_name" | "product" => {
                let adj = pick(rng, PRODUCT_ADJECTIVES);
                let material = pick(rng, PRODUCT_MATERIALS);
                let noun = pick(rng, PRODUCT_NOUNS);
                format!("{adj} {material} {noun}")
            }
            "state" => pick(rng, US_STATES).to_string(),
            "country" => pick(rng, COUNTRIES).to_string(),
            "zip_code" | "zipcode" | "postal_code" => {
                let code = rng.next_u32() % 100000;
                format!("{code:05}")
            }
            "url" => {
                let word = pick(rng, WORDS);
                let tld = pick(rng, TLDS);
                format!("https://{word}.{tld}")
            }
            "domain" => {
                let word = pick(rng, WORDS);
                let tld = pick(rng, TLDS);
                format!("{word}.{tld}")
            }
            "ipv4" | "ip_address" => {
                let a = rng.next_u32() % 256;
                let b = rng.next_u32() % 256;
                let c = rng.next_u32() % 256;
                let d = rng.next_u32() % 256;
                format!("{a}.{b}.{c}.{d}")
            }
            "ipv6" => {
                let mut parts = [0u16; 8];
                for p in &mut parts {
                    *p = (rng.next_u32() & 0xFFFF) as u16;
                }
                format!(
                    "{:04x}:{:04x}:{:04x}:{:04x}:{:04x}:{:04x}:{:04x}:{:04x}",
                    parts[0], parts[1], parts[2], parts[3],
                    parts[4], parts[5], parts[6], parts[7]
                )
            }
            "date" => {
                // Generate a random ISO date within configured or default range
                let (start_days, end_days) = self.date_range();
                let range = (end_days - start_days + 1).max(1) as u32;
                let day_offset = rng.next_u32() % range;
                let (y, m, d) = days_to_ymd(start_days + day_offset as i64);
                format!("{y:04}-{m:02}-{d:02}")
            }
            "datetime" | "timestamp" => {
                // ISO datetime with random time within configured or default range
                let (start_days, end_days) = self.date_range();
                let range = (end_days - start_days + 1).max(1) as u32;
                let day_offset = rng.next_u32() % range;
                let (y, m, d) = days_to_ymd(start_days + day_offset as i64);
                let h = rng.next_u32() % 24;
                let min = rng.next_u32() % 60;
                let s = rng.next_u32() % 60;
                format!("{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{s:02}")
            }
            "color" => pick(rng, COLORS).to_string(),
            "hex_color" => {
                let r = rng.next_u32() % 256;
                let g = rng.next_u32() % 256;
                let b = rng.next_u32() % 256;
                format!("#{r:02x}{g:02x}{b:02x}")
            }
            "hex_string" => {
                // Generate a random hex string; length from first arg (default 32)
                let len = self.args.first()
                    .and_then(|v| match v {
                        knit_core::Value::Int(n) if *n > 0 => Some((*n as usize).min(1024)),
                        _ => None,
                    })
                    .unwrap_or(32);
                let byte_count = (len + 1) / 2;
                let mut bytes = vec![0u8; byte_count];
                rng.fill_bytes(&mut bytes);
                let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
                hex[..len].to_string()
            }
            "paragraph" => {
                let sentence_count = 2 + (rng.next_u32() % 4) as usize; // 2..=5
                let mut para = String::with_capacity(sentence_count * 50);
                for i in 0..sentence_count {
                    if i > 0 {
                        para.push(' ');
                    }
                    let word_count = 4 + (rng.next_u32() % 8) as usize; // 4..=11
                    for j in 0..word_count {
                        if j > 0 {
                            para.push(' ');
                        }
                        let w = pick(rng, WORDS);
                        if j == 0 {
                            let mut chars = w.chars();
                            if let Some(c) = chars.next() {
                                for uc in c.to_uppercase() {
                                    para.push(uc);
                                }
                                para.push_str(chars.as_str());
                            }
                        } else {
                            para.push_str(w);
                        }
                    }
                    para.push('.');
                }
                para
            }
            "title" => {
                let word_count = 2 + (rng.next_u32() % 4) as usize; // 2..=5
                let mut t = String::with_capacity(word_count * 7);
                for i in 0..word_count {
                    if i > 0 {
                        t.push(' ');
                    }
                    let w = pick(rng, WORDS);
                    // Title case: capitalize first char of each word
                    let mut chars = w.chars();
                    if let Some(c) = chars.next() {
                        for uc in c.to_uppercase() {
                            t.push(uc);
                        }
                        t.push_str(chars.as_str());
                    }
                }
                t
            }
            unknown => {
                if !self.warned.swap(true, Ordering::Relaxed) {
                    tracing::warn!(
                        category = unknown,
                        "unknown faker category, returning category name as constant"
                    );
                }
                unknown.to_string()
            }
        }
    }
}

impl FieldGenerator for FakerGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, _ctx: &GenContext) -> ArrayRef {
        match self.category.as_str() {
            "datetime" | "timestamp" => {
                // Generate timestamp as nanoseconds since epoch
                let (start_days, end_days) = self.date_range();
                let values: Vec<i64> = (0..count)
                    .map(|_| {
                        let range = (end_days - start_days + 1).max(1) as u32;
                        let day_offset = rng.next_u32() % range;
                        let days = start_days + day_offset as i64;
                        let h = (rng.next_u32() % 24) as i64;
                        let min = (rng.next_u32() % 60) as i64;
                        let s = (rng.next_u32() % 60) as i64;
                        // nanoseconds since epoch
                        days * 86_400_000_000_000 + h * 3_600_000_000_000
                            + min * 60_000_000_000 + s * 1_000_000_000
                    })
                    .collect();
                Arc::new(TimestampNanosecondArray::from(values))
            }
            "date" => {
                // Generate date as days since epoch
                let (start_days, end_days) = self.date_range();
                let values: Vec<i32> = (0..count)
                    .map(|_| {
                        let range = (end_days - start_days + 1).max(1) as u32;
                        let day_offset = rng.next_u32() % range;
                        (start_days + day_offset as i64) as i32
                    })
                    .collect();
                Arc::new(Date32Array::from(values))
            }
            _ => {
                let values: Vec<String> =
                    (0..count).map(|_| self.generate_one(rng)).collect();
                Arc::new(StringArray::from(
                    values.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                ))
            }
        }
    }

    fn output_type(&self) -> DataType {
        match self.category.as_str() {
            "datetime" | "timestamp" => {
                DataType::Timestamp(TimeUnit::Nanosecond, None)
            }
            "date" => DataType::Date32,
            _ => DataType::Utf8,
        }
    }
}

/// Convert a civil date to days since Unix epoch (1970-01-01).
fn days_from_epoch(year: i32, month: u32, day: u32) -> i64 {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let y = if month <= 2 { year as i64 - 1 } else { year as i64 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let m = month as u32;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146097 + doe as i64) - 719468
}

/// Convert days since Unix epoch back to (year, month, day).
fn days_to_ymd(days: i64) -> (i32, u32, u32) {
    // Inverse of days_from_epoch
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year as i32, m, d)
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
        GenContext::new(map, 0, 0, 1, "test")
    }

    fn gen(category: &str, count: usize, seed: u64) -> ArrayRef {
        let g = FakerGenerator::new(category.into(), "en_US".into(), vec![]);
        let ctx = make_ctx();
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        g.generate(&mut rng, count, &ctx)
    }

    fn gen_with_args(category: &str, args: Vec<knit_core::Value>, count: usize, seed: u64) -> ArrayRef {
        let g = FakerGenerator::new(category.into(), "en_US".into(), args);
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
        let g = FakerGenerator::new("email".into(), "en_US".into(), vec![]);
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

    #[test]
    fn state_produces_known_state() {
        let arr = gen("state", 50, 20);
        for v in strings(&arr) {
            assert!(
                super::US_STATES.contains(&v.as_str()),
                "state should be from US_STATES list: {v}"
            );
        }
    }

    #[test]
    fn country_produces_known_country() {
        let arr = gen("country", 50, 21);
        for v in strings(&arr) {
            assert!(
                super::COUNTRIES.contains(&v.as_str()),
                "country should be from COUNTRIES list: {v}"
            );
        }
    }

    #[test]
    fn zip_code_five_digits() {
        let arr = gen("zip_code", 100, 22);
        for v in strings(&arr) {
            assert_eq!(v.len(), 5, "zip_code should be 5 chars: {v}");
            assert!(v.chars().all(|c| c.is_ascii_digit()), "zip_code should be digits: {v}");
        }
    }

    #[test]
    fn zip_code_aliases() {
        // All aliases should produce 5-digit codes
        for alias in &["zip_code", "zipcode", "postal_code"] {
            let arr = gen(alias, 10, 23);
            for v in strings(&arr) {
                assert_eq!(v.len(), 5, "{alias} should produce 5-digit code: {v}");
            }
        }
    }

    #[test]
    fn url_format() {
        let arr = gen("url", 50, 24);
        for v in strings(&arr) {
            assert!(v.starts_with("https://"), "url should start with https://: {v}");
            assert!(v.contains('.'), "url should contain a dot: {v}");
        }
    }

    #[test]
    fn domain_format() {
        let arr = gen("domain", 50, 25);
        for v in strings(&arr) {
            assert!(!v.starts_with("https://"), "domain should not have scheme: {v}");
            assert!(v.contains('.'), "domain should contain a dot: {v}");
        }
    }

    #[test]
    fn ipv4_format() {
        let arr = gen("ipv4", 50, 26);
        for v in strings(&arr) {
            let octets: Vec<&str> = v.split('.').collect();
            assert_eq!(octets.len(), 4, "ipv4 should have 4 octets: {v}");
            for octet in &octets {
                let n: u32 = octet.parse().unwrap_or(999);
                assert!(n < 256, "ipv4 octet out of range: {v}");
            }
        }
    }

    #[test]
    fn ip_address_alias() {
        let arr = gen("ip_address", 10, 27);
        for v in strings(&arr) {
            assert_eq!(v.split('.').count(), 4, "ip_address should produce ipv4: {v}");
        }
    }

    #[test]
    fn ipv6_format() {
        let arr = gen("ipv6", 50, 28);
        for v in strings(&arr) {
            let groups: Vec<&str> = v.split(':').collect();
            assert_eq!(groups.len(), 8, "ipv6 should have 8 groups: {v}");
            for g in &groups {
                assert_eq!(g.len(), 4, "ipv6 group should be 4 hex chars: {v}");
                assert!(
                    g.chars().all(|c| c.is_ascii_hexdigit()),
                    "ipv6 group should be hex: {v}"
                );
            }
        }
    }

    #[test]
    fn color_from_list() {
        let arr = gen("color", 50, 29);
        for v in strings(&arr) {
            assert!(
                super::COLORS.contains(&v.as_str()),
                "color should be from COLORS list: {v}"
            );
        }
    }

    #[test]
    fn hex_color_format() {
        let arr = gen("hex_color", 50, 30);
        for v in strings(&arr) {
            assert!(v.starts_with('#'), "hex_color should start with #: {v}");
            assert_eq!(v.len(), 7, "hex_color should be 7 chars: {v}");
            assert!(
                v[1..].chars().all(|c| c.is_ascii_hexdigit()),
                "hex_color should be hex digits: {v}"
            );
        }
    }

    #[test]
    fn paragraph_multiple_sentences() {
        let arr = gen("paragraph", 20, 31);
        for v in strings(&arr) {
            assert!(v.ends_with('.'), "paragraph should end with period: {v}");
            let sentence_count = v.matches('.').count();
            assert!(sentence_count >= 2, "paragraph should have >=2 sentences: {v}");
        }
    }

    #[test]
    fn title_title_case() {
        let arr = gen("title", 50, 32);
        for v in strings(&arr) {
            assert!(!v.is_empty(), "title should not be empty");
            // Each word should start with uppercase
            for word in v.split_whitespace() {
                assert!(
                    word.chars().next().unwrap().is_uppercase(),
                    "title word should be capitalized: {word} in {v}"
                );
            }
        }
    }

    #[test]
    fn hex_string_default_length() {
        let arr = gen("hex_string", 50, 40);
        for v in strings(&arr) {
            assert_eq!(v.len(), 32, "default hex_string should be 32 chars: {v}");
            assert!(
                v.chars().all(|c| c.is_ascii_hexdigit()),
                "hex_string should be hex digits: {v}"
            );
        }
    }

    #[test]
    fn hex_string_custom_length() {
        let arr = gen_with_args("hex_string", vec![knit_core::Value::Int(40)], 50, 41);
        for v in strings(&arr) {
            assert_eq!(v.len(), 40, "hex_string with arg 40 should be 40 chars: {v}");
            assert!(
                v.chars().all(|c| c.is_ascii_hexdigit()),
                "hex_string should be hex digits: {v}"
            );
        }
    }

    #[test]
    fn datetime_produces_timestamp_array() {
        let g = FakerGenerator::new("datetime".into(), "en_US".into(), vec![]);
        assert_eq!(
            g.output_type(),
            DataType::Timestamp(arrow::datatypes::TimeUnit::Nanosecond, None)
        );
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = make_ctx();
        let arr = g.generate(&mut rng, 10, &ctx);
        assert_eq!(arr.len(), 10);
        assert_eq!(
            *arr.data_type(),
            DataType::Timestamp(arrow::datatypes::TimeUnit::Nanosecond, None)
        );
    }

    #[test]
    fn date_produces_date32_array() {
        let g = FakerGenerator::new("date".into(), "en_US".into(), vec![]);
        assert_eq!(g.output_type(), DataType::Date32);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = make_ctx();
        let arr = g.generate(&mut rng, 10, &ctx);
        assert_eq!(arr.len(), 10);
        assert_eq!(*arr.data_type(), DataType::Date32);
    }
}
