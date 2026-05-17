//! Faker-style realistic data generator.
//!
//! Produces human-readable synthetic values — names, emails, addresses, phone
//! numbers, and more — by randomly sampling from embedded word lists. No
//! external faker crate is required; all data is self-contained.
//!
//! Determinism is guaranteed for a given [`Rng`] state so that the same
//! seed reproduces identical datasets across runs.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use arrow::array::{ArrayRef, Date32Array, StringArray, TimestampNanosecondArray};
use arrow::datatypes::{DataType, TimeUnit};
use rand::Rng;

use crate::r#gen::context::GenContext;
use crate::r#gen::traits::FieldGenerator;

// ---------------------------------------------------------------------------
// Word lists
// ---------------------------------------------------------------------------

/// Common first names (~150 entries).
static FIRST_NAMES: &[&str] = &[
    "James",
    "Mary",
    "Robert",
    "Patricia",
    "John",
    "Jennifer",
    "Michael",
    "Linda",
    "David",
    "Elizabeth",
    "William",
    "Barbara",
    "Richard",
    "Susan",
    "Joseph",
    "Jessica",
    "Thomas",
    "Sarah",
    "Charles",
    "Karen",
    "Christopher",
    "Lisa",
    "Daniel",
    "Nancy",
    "Matthew",
    "Betty",
    "Anthony",
    "Margaret",
    "Mark",
    "Sandra",
    "Donald",
    "Ashley",
    "Steven",
    "Kimberly",
    "Paul",
    "Emily",
    "Andrew",
    "Donna",
    "Joshua",
    "Michelle",
    "Kenneth",
    "Carol",
    "Kevin",
    "Amanda",
    "Brian",
    "Dorothy",
    "George",
    "Melissa",
    "Timothy",
    "Deborah",
    "Ronald",
    "Stephanie",
    "Edward",
    "Rebecca",
    "Jason",
    "Sharon",
    "Jeffrey",
    "Laura",
    "Ryan",
    "Cynthia",
    "Jacob",
    "Kathleen",
    "Gary",
    "Amy",
    "Nicholas",
    "Angela",
    "Eric",
    "Shirley",
    "Jonathan",
    "Anna",
    "Stephen",
    "Brenda",
    "Larry",
    "Pamela",
    "Justin",
    "Emma",
    "Scott",
    "Nicole",
    "Brandon",
    "Helen",
    "Benjamin",
    "Samantha",
    "Samuel",
    "Katherine",
    "Raymond",
    "Christine",
    "Gregory",
    "Debra",
    "Frank",
    "Rachel",
    "Alexander",
    "Carolyn",
    "Patrick",
    "Janet",
    "Jack",
    "Catherine",
    "Dennis",
    "Maria",
    "Jerry",
    "Heather",
    "Tyler",
    "Diane",
    "Aaron",
    "Ruth",
    "Jose",
    "Julie",
    "Adam",
    "Olivia",
    "Nathan",
    "Joyce",
    "Henry",
    "Virginia",
    "Peter",
    "Victoria",
    "Zachary",
    "Kelly",
    "Douglas",
    "Lauren",
    "Harold",
    "Christina",
    "Carl",
    "Joan",
    "Arthur",
    "Evelyn",
    "Gerald",
    "Judith",
    "Roger",
    "Megan",
    "Keith",
    "Andrea",
    "Albert",
    "Cheryl",
    "Jeremy",
    "Hannah",
    "Terry",
    "Jacqueline",
    "Sean",
    "Martha",
    "Austin",
    "Gloria",
    "Randy",
    "Teresa",
    "Howard",
    "Ann",
    "Eugene",
    "Sara",
    "Russell",
    "Madison",
    "Louis",
    "Frances",
    "Philip",
    "Kathryn",
    "Alice",
    "Carlos",
    "Wei",
    "Yuki",
    "Omar",
    "Fatima",
    "Ravi",
    "Priya",
];

/// Common last names (~150 entries).
static LAST_NAMES: &[&str] = &[
    "Smith",
    "Johnson",
    "Williams",
    "Brown",
    "Jones",
    "Garcia",
    "Miller",
    "Davis",
    "Rodriguez",
    "Martinez",
    "Hernandez",
    "Lopez",
    "Gonzalez",
    "Wilson",
    "Anderson",
    "Thomas",
    "Taylor",
    "Moore",
    "Jackson",
    "Martin",
    "Lee",
    "Perez",
    "Thompson",
    "White",
    "Harris",
    "Sanchez",
    "Clark",
    "Ramirez",
    "Lewis",
    "Robinson",
    "Walker",
    "Young",
    "Allen",
    "King",
    "Wright",
    "Scott",
    "Torres",
    "Nguyen",
    "Hill",
    "Flores",
    "Green",
    "Adams",
    "Nelson",
    "Baker",
    "Hall",
    "Rivera",
    "Campbell",
    "Mitchell",
    "Carter",
    "Roberts",
    "Gomez",
    "Phillips",
    "Evans",
    "Turner",
    "Diaz",
    "Parker",
    "Cruz",
    "Edwards",
    "Collins",
    "Reyes",
    "Stewart",
    "Morris",
    "Morales",
    "Murphy",
    "Cook",
    "Rogers",
    "Gutierrez",
    "Ortiz",
    "Morgan",
    "Cooper",
    "Peterson",
    "Bailey",
    "Reed",
    "Kelly",
    "Howard",
    "Ramos",
    "Kim",
    "Cox",
    "Ward",
    "Richardson",
    "Watson",
    "Brooks",
    "Chavez",
    "Wood",
    "James",
    "Bennett",
    "Gray",
    "Mendoza",
    "Ruiz",
    "Hughes",
    "Price",
    "Alvarez",
    "Castillo",
    "Sanders",
    "Patel",
    "Myers",
    "Long",
    "Ross",
    "Foster",
    "Jimenez",
    "Powell",
    "Jenkins",
    "Perry",
    "Russell",
    "Sullivan",
    "Bell",
    "Coleman",
    "Butler",
    "Henderson",
    "Barnes",
    "Gonzales",
    "Fisher",
    "Vasquez",
    "Simmons",
    "Griffin",
    "Marshall",
    "Owens",
    "Harrison",
    "Fernandez",
    "McDonald",
    "Woods",
    "Washington",
    "Kennedy",
    "Wells",
    "Vargas",
    "Henry",
    "Chen",
    "Freeman",
    "Webb",
    "Tucker",
    "Burns",
    "Crawford",
    "Olson",
    "Simpson",
    "Porter",
    "Hunter",
    "Gordon",
    "Mendez",
    "Silva",
    "Shaw",
    "Snyder",
    "Mason",
    "Dixon",
    "Munoz",
    "Hunt",
    "Hicks",
    "Holmes",
    "Palmer",
    "Wagner",
    "Black",
    "Robertson",
    "Boyd",
];

/// Common English words for `word` / `sentence` generation (~200 entries).
static WORDS: &[&str] = &[
    "time",
    "year",
    "people",
    "way",
    "day",
    "man",
    "woman",
    "child",
    "world",
    "life",
    "hand",
    "part",
    "place",
    "case",
    "week",
    "company",
    "system",
    "program",
    "question",
    "work",
    "government",
    "number",
    "night",
    "point",
    "home",
    "water",
    "room",
    "mother",
    "area",
    "money",
    "story",
    "fact",
    "month",
    "lot",
    "right",
    "study",
    "book",
    "eye",
    "job",
    "word",
    "business",
    "issue",
    "side",
    "kind",
    "head",
    "house",
    "service",
    "friend",
    "father",
    "power",
    "hour",
    "game",
    "line",
    "end",
    "member",
    "law",
    "car",
    "city",
    "community",
    "name",
    "president",
    "team",
    "minute",
    "idea",
    "body",
    "information",
    "back",
    "parent",
    "face",
    "others",
    "level",
    "office",
    "door",
    "health",
    "person",
    "art",
    "war",
    "history",
    "party",
    "result",
    "change",
    "morning",
    "reason",
    "research",
    "girl",
    "guy",
    "moment",
    "air",
    "teacher",
    "force",
    "education",
    "great",
    "new",
    "good",
    "old",
    "small",
    "long",
    "large",
    "high",
    "different",
    "little",
    "local",
    "social",
    "important",
    "national",
    "young",
    "possible",
    "public",
    "real",
    "big",
    "early",
    "able",
    "political",
    "major",
    "special",
    "human",
    "certain",
    "sure",
    "true",
    "free",
    "strong",
    "open",
    "available",
    "likely",
    "clear",
    "simple",
    "recent",
    "common",
    "economic",
    "current",
    "similar",
    "natural",
    "physical",
    "dark",
    "hard",
    "single",
    "whole",
    "happy",
    "serious",
    "ready",
    "full",
    "short",
    "better",
    "best",
    "fast",
    "heavy",
    "main",
    "final",
    "general",
    "light",
    "deep",
    "past",
    "close",
    "private",
    "poor",
    "easy",
    "direct",
    "bright",
    "foreign",
    "quiet",
    "rich",
    "modern",
    "global",
    "safe",
    "warm",
    "cold",
    "thin",
    "wide",
    "sharp",
    "smooth",
    "soft",
    "sweet",
    "wild",
    "young",
    "basic",
    "visual",
    "active",
    "complex",
    "critical",
    "digital",
    "narrow",
    "normal",
    "rare",
    "regular",
    "secure",
    "standard",
    "central",
    "creative",
    "primary",
    "proper",
    "unique",
    "useful",
    "vast",
    "vital",
    "broad",
    "exact",
    "fair",
    "firm",
    "formal",
    "grand",
];

/// Common city names (~150 entries).
static CITIES: &[&str] = &[
    "New York",
    "Los Angeles",
    "Chicago",
    "Houston",
    "Phoenix",
    "Philadelphia",
    "San Antonio",
    "San Diego",
    "Dallas",
    "San Jose",
    "Austin",
    "Jacksonville",
    "Fort Worth",
    "Columbus",
    "Charlotte",
    "Indianapolis",
    "San Francisco",
    "Seattle",
    "Denver",
    "Nashville",
    "Oklahoma City",
    "El Paso",
    "Washington",
    "Boston",
    "Las Vegas",
    "Portland",
    "Memphis",
    "Louisville",
    "Baltimore",
    "Milwaukee",
    "Albuquerque",
    "Tucson",
    "Fresno",
    "Mesa",
    "Sacramento",
    "Atlanta",
    "Kansas City",
    "Colorado Springs",
    "Omaha",
    "Raleigh",
    "Miami",
    "Minneapolis",
    "Tampa",
    "New Orleans",
    "Arlington",
    "Cleveland",
    "Bakersfield",
    "Aurora",
    "Anaheim",
    "Honolulu",
    "Santa Ana",
    "Riverside",
    "Corpus Christi",
    "Lexington",
    "Pittsburgh",
    "Anchorage",
    "Stockton",
    "Cincinnati",
    "Saint Paul",
    "Toledo",
    "Newark",
    "Greensboro",
    "Buffalo",
    "Plano",
    "Lincoln",
    "Henderson",
    "Fort Wayne",
    "Jersey City",
    "Chandler",
    "Norfolk",
    "Durham",
    "Madison",
    "Lubbock",
    "Irvine",
    "Winston-Salem",
    "Glendale",
    "Garland",
    "Hialeah",
    "Laredo",
    "Boise",
    "Richmond",
    "Spokane",
    "Baton Rouge",
    "Des Moines",
    "Birmingham",
    "Modesto",
    "Rochester",
    "Tacoma",
    "Fontana",
    "Oxnard",
    "Moreno Valley",
    "Fayetteville",
    "Huntington Beach",
    "Salt Lake City",
    "Grand Rapids",
    "Tallahassee",
    "Worcester",
    "Knoxville",
    "Akron",
    "Brownsville",
    "Newport News",
    "Sioux Falls",
    "Chattanooga",
    "Providence",
    "Wichita",
    "Savannah",
    "Little Rock",
    "Dayton",
    "Reno",
    "Peoria",
    "Tempe",
    "Eugene",
    "Hampton",
    "Salem",
    "Gilbert",
    "Surprise",
    "Joliet",
    "Naperville",
    "Bridgeport",
    "Paterson",
    "Topeka",
    "Macon",
    "Lakewood",
    "Odessa",
    "Pomona",
    "Escondido",
    "Sunnyvale",
    "Pasadena",
    "Hayward",
    "Torrance",
    "Visalia",
    "Roseville",
    "Thornton",
    "Sterling Heights",
    "Carrollton",
    "Denton",
    "Midland",
    "Murfreesboro",
    "West Valley City",
    "Lewisville",
    "Waco",
    "Allen",
    "Sparks",
    "Pueblo",
    "London",
    "Paris",
    "Tokyo",
    "Berlin",
    "Sydney",
    "Toronto",
];

/// Company name parts for composing realistic company names.
static COMPANY_PREFIXES: &[&str] = &[
    "Acme",
    "Global",
    "First",
    "National",
    "Pacific",
    "Atlantic",
    "Summit",
    "Pinnacle",
    "Vertex",
    "Apex",
    "Prime",
    "Sterling",
    "Noble",
    "Quantum",
    "Nexus",
    "Vanguard",
    "Horizon",
    "Eclipse",
    "Zenith",
    "Titan",
    "Phoenix",
    "Atlas",
    "Forge",
    "Pulse",
    "Spark",
    "Core",
    "Nova",
    "Crest",
    "Peak",
    "Bridge",
    "Shield",
    "Beacon",
    "Evergreen",
    "Silver",
    "Golden",
    "Crystal",
    "Iron",
    "Blue",
    "Red",
    "Alpha",
    "Omega",
    "Delta",
    "Sigma",
    "Vector",
    "Metro",
    "Urban",
    "Coastal",
    "Northern",
    "Southern",
    "Western",
    "Eastern",
    "Central",
    "United",
    "Allied",
    "Pioneer",
    "Liberty",
    "Heritage",
    "Legacy",
    "Keystone",
    "Granite",
    "Maple",
    "Oak",
    "Cedar",
    "Sage",
];

/// Company name suffixes.
static COMPANY_SUFFIXES: &[&str] = &[
    "Corp",
    "Inc",
    "LLC",
    "Group",
    "Solutions",
    "Technologies",
    "Systems",
    "Industries",
    "Enterprises",
    "Services",
    "Partners",
    "Associates",
    "Holdings",
    "Dynamics",
    "Consulting",
    "Labs",
    "Ventures",
    "Capital",
    "Networks",
    "Digital",
    "Analytics",
    "Global",
    "International",
    "Research",
    "Financial",
    "Logistics",
    "Media",
    "Health",
    "Bio",
    "Soft",
];

/// US state names (~50 entries).
static US_STATES: &[&str] = &[
    "Alabama",
    "Alaska",
    "Arizona",
    "Arkansas",
    "California",
    "Colorado",
    "Connecticut",
    "Delaware",
    "Florida",
    "Georgia",
    "Hawaii",
    "Idaho",
    "Illinois",
    "Indiana",
    "Iowa",
    "Kansas",
    "Kentucky",
    "Louisiana",
    "Maine",
    "Maryland",
    "Massachusetts",
    "Michigan",
    "Minnesota",
    "Mississippi",
    "Missouri",
    "Montana",
    "Nebraska",
    "Nevada",
    "New Hampshire",
    "New Jersey",
    "New Mexico",
    "New York",
    "North Carolina",
    "North Dakota",
    "Ohio",
    "Oklahoma",
    "Oregon",
    "Pennsylvania",
    "Rhode Island",
    "South Carolina",
    "South Dakota",
    "Tennessee",
    "Texas",
    "Utah",
    "Vermont",
    "Virginia",
    "Washington",
    "West Virginia",
    "Wisconsin",
    "Wyoming",
];

/// Country names (~60 entries).
static COUNTRIES: &[&str] = &[
    "United States",
    "Canada",
    "United Kingdom",
    "France",
    "Germany",
    "Italy",
    "Spain",
    "Australia",
    "Japan",
    "China",
    "India",
    "Brazil",
    "Mexico",
    "South Korea",
    "Russia",
    "Netherlands",
    "Switzerland",
    "Sweden",
    "Norway",
    "Denmark",
    "Finland",
    "Belgium",
    "Austria",
    "Ireland",
    "Portugal",
    "Poland",
    "Czech Republic",
    "Greece",
    "Turkey",
    "Israel",
    "South Africa",
    "Egypt",
    "Nigeria",
    "Kenya",
    "Argentina",
    "Colombia",
    "Chile",
    "Peru",
    "Thailand",
    "Vietnam",
    "Indonesia",
    "Philippines",
    "Malaysia",
    "Singapore",
    "New Zealand",
    "Saudi Arabia",
    "United Arab Emirates",
    "Pakistan",
    "Bangladesh",
    "Taiwan",
    "Hong Kong",
    "Ukraine",
    "Romania",
    "Hungary",
    "Croatia",
    "Serbia",
    "Slovakia",
    "Slovenia",
    "Iceland",
    "Luxembourg",
];

/// Color names for `color` method.
static COLORS: &[&str] = &[
    "red",
    "blue",
    "green",
    "yellow",
    "orange",
    "purple",
    "pink",
    "brown",
    "black",
    "white",
    "gray",
    "cyan",
    "magenta",
    "lime",
    "teal",
    "navy",
    "maroon",
    "olive",
    "aqua",
    "coral",
    "crimson",
    "gold",
    "indigo",
    "ivory",
    "khaki",
    "lavender",
    "orchid",
    "plum",
    "salmon",
    "sienna",
    "silver",
    "tan",
    "turquoise",
    "violet",
    "wheat",
    "beige",
    "azure",
    "chartreuse",
    "fuchsia",
    "scarlet",
];

/// Top-level domains for URL generation.
static TLDS: &[&str] = &["com", "org", "net", "io", "dev", "co", "app", "tech"];

/// Email domains.
static DOMAINS: &[&str] = &[
    "example.com",
    "mail.com",
    "email.com",
    "inbox.com",
    "webmail.com",
    "fastmail.net",
    "proton.me",
    "outlook.com",
    "postal.io",
    "letters.org",
    "testmail.com",
    "demo.net",
    "sample.org",
    "placeholder.com",
    "mailbox.io",
    "fakemail.com",
    "tempmail.net",
    "quickmail.org",
    "simplemail.com",
    "postoffice.net",
];

/// Product adjectives for `product_name` generation (~60 entries).
static PRODUCT_ADJECTIVES: &[&str] = &[
    "Ultra",
    "Pro",
    "Classic",
    "Premium",
    "Essential",
    "Advanced",
    "Compact",
    "Deluxe",
    "Elite",
    "Smart",
    "Eco",
    "Turbo",
    "Slim",
    "Heavy-Duty",
    "Portable",
    "Wireless",
    "Digital",
    "Organic",
    "Natural",
    "Industrial",
    "Precision",
    "Royal",
    "Mega",
    "Mini",
    "Supreme",
    "Rapid",
    "Silent",
    "Flex",
    "Hyper",
    "Quantum",
    "Vintage",
    "Modern",
    "Rugged",
    "Sleek",
    "Ergonomic",
    "Thermal",
    "Solar",
    "Vivid",
    "Arctic",
    "Tropic",
    "Nordic",
    "Alpine",
    "Coastal",
    "Urban",
    "Rustic",
    "Luxe",
    "Atomic",
    "Stealth",
    "Summit",
    "Apex",
    "Prime",
    "Core",
    "Nova",
    "Volt",
    "Aero",
    "Titan",
    "Zenith",
    "Craft",
    "Studio",
    "Trek",
];

/// Product materials/descriptors for `product_name` generation (~50 entries).
static PRODUCT_MATERIALS: &[&str] = &[
    "Steel", "Bamboo", "Cotton", "Granite", "Leather", "Silk", "Rubber", "Bronze", "Ceramic",
    "Carbon", "Titanium", "Copper", "Wooden", "Plastic", "Glass", "Marble", "Linen", "Chrome",
    "Velvet", "Nylon", "Suede", "Denim", "Canvas", "Aluminum", "Iron", "Brass", "Nickel",
    "Platinum", "Cobalt", "Graphite", "Quartz", "Jade", "Ivory", "Ebony", "Walnut", "Birch",
    "Maple", "Cedar", "Pine", "Teak", "Acrylic", "Polymer", "Fiber", "Mesh", "Woven", "Forged",
    "Cast", "Polished", "Matte", "Satin",
];

/// Product nouns for `product_name` generation (~80 entries).
static PRODUCT_NOUNS: &[&str] = &[
    "Headphones",
    "Keyboard",
    "Chair",
    "Lamp",
    "Backpack",
    "Wallet",
    "Watch",
    "Shoes",
    "Blender",
    "Towels",
    "Gloves",
    "Jacket",
    "Bottle",
    "Speaker",
    "Camera",
    "Tablet",
    "Pan",
    "Socks",
    "Hat",
    "Bike",
    "Ball",
    "Table",
    "Shirt",
    "Pants",
    "Mouse",
    "Monitor",
    "Bench",
    "Pillow",
    "Candle",
    "Knife",
    "Mug",
    "Clock",
    "Brush",
    "Blanket",
    "Desk",
    "Shelf",
    "Cabinet",
    "Rug",
    "Vase",
    "Frame",
    "Cooler",
    "Grill",
    "Mixer",
    "Toaster",
    "Iron",
    "Drill",
    "Wrench",
    "Pliers",
    "Scarf",
    "Belt",
    "Boots",
    "Sandals",
    "Hoodie",
    "Vest",
    "Tie",
    "Ring",
    "Earbuds",
    "Charger",
    "Router",
    "Printer",
    "Scanner",
    "Tripod",
    "Lens",
    "Stand",
    "Mat",
    "Rack",
    "Hook",
    "Tray",
    "Basket",
    "Bin",
    "Crate",
    "Box",
    "Pad",
    "Case",
    "Cover",
    "Strap",
    "Clip",
    "Band",
    "Grip",
    "Mount",
];

/// Street name bases.
static STREET_NAMES: &[&str] = &[
    "Main",
    "Oak",
    "Pine",
    "Maple",
    "Cedar",
    "Elm",
    "Park",
    "Lake",
    "Hill",
    "Walnut",
    "Sunset",
    "Ridge",
    "River",
    "Spring",
    "Valley",
    "Forest",
    "Meadow",
    "Brook",
    "Highland",
    "Willow",
    "Cherry",
    "Birch",
    "Ash",
    "Laurel",
    "Rose",
    "Magnolia",
    "Chestnut",
    "Spruce",
    "Poplar",
    "Sycamore",
    "Olive",
    "Peach",
    "Vine",
    "Church",
    "School",
    "Mill",
    "Bridge",
    "Market",
    "Washington",
    "Jefferson",
    "Lincoln",
    "Franklin",
    "Adams",
    "Jackson",
    "Harrison",
    "Madison",
    "Monroe",
    "Grant",
];

/// Street suffixes.
static STREET_SUFFIXES: &[&str] = &[
    "St", "Ave", "Blvd", "Dr", "Ln", "Rd", "Ct", "Pl", "Way", "Cir", "Pkwy", "Ter",
];

// ── Person extras ──────────────────────────────────────────────────────

static NAME_PREFIXES: &[&str] = &["Mr.", "Mrs.", "Ms.", "Dr.", "Prof.", "Rev.", "Hon.", "Sir"];

static NAME_SUFFIXES: &[&str] = &[
    "Jr.", "Sr.", "II", "III", "IV", "MD", "PhD", "Esq.", "DDS", "DVM",
];

// ── Internet extras ────────────────────────────────────────────────────

static USER_AGENTS: &[&str] = &[
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_2) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.2 Safari/605.1.15",
    "Mozilla/5.0 (X11; Linux x86_64; rv:121.0) Gecko/20100101 Firefox/121.0",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36 Edg/120.0.0.0",
    "Mozilla/5.0 (iPhone; CPU iPhone OS 17_2 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.2 Mobile/15E148 Safari/604.1",
    "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Mobile Safari/537.36",
    "Mozilla/5.0 (iPad; CPU OS 17_2 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.2 Mobile/15E148 Safari/604.1",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:121.0) Gecko/20100101 Firefox/121.0",
];

// ── Finance ────────────────────────────────────────────────────────────

static COUNTRY_CODES: &[&str] = &[
    "US", "GB", "DE", "FR", "JP", "CN", "AU", "CA", "BR", "IN", "IT", "ES", "MX", "KR", "RU", "NL",
    "SE", "CH", "AT", "BE", "NO", "DK", "FI", "PL", "PT", "IE", "NZ", "SG", "HK", "TW",
];

static CURRENCY_CODES: &[&str] = &[
    "USD", "EUR", "GBP", "JPY", "AUD", "CAD", "CHF", "CNY", "SEK", "NZD", "MXN", "SGD", "HKD",
    "NOK", "KRW", "TRY", "INR", "RUB", "BRL", "ZAR", "DKK", "PLN", "TWD", "THB", "IDR",
];

// ── Datetime extras ────────────────────────────────────────────────────

static MONTHS: &[&str] = &[
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

static WEEKDAYS: &[&str] = &[
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

static TIMEZONES: &[&str] = &[
    "America/New_York",
    "America/Chicago",
    "America/Denver",
    "America/Los_Angeles",
    "Europe/London",
    "Europe/Berlin",
    "Europe/Paris",
    "Europe/Moscow",
    "Asia/Tokyo",
    "Asia/Shanghai",
    "Asia/Kolkata",
    "Asia/Dubai",
    "Australia/Sydney",
    "Pacific/Auckland",
    "America/Sao_Paulo",
    "Africa/Cairo",
    "America/Toronto",
    "Europe/Amsterdam",
    "Asia/Singapore",
    "Asia/Seoul",
];

// ── File ───────────────────────────────────────────────────────────────

static FILE_EXTENSIONS: &[&str] = &[
    "pdf", "doc", "docx", "xls", "xlsx", "csv", "txt", "json", "xml", "png", "jpg", "gif", "mp4",
    "mp3", "zip", "gz", "tar", "html", "css", "js", "ts", "py", "rs", "go", "java", "cpp", "h",
    "md", "yaml",
];

static MIME_TYPES: &[&str] = &[
    "application/pdf",
    "application/json",
    "application/xml",
    "application/zip",
    "application/octet-stream",
    "text/plain",
    "text/html",
    "text/css",
    "text/csv",
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/svg+xml",
    "audio/mpeg",
    "video/mp4",
];

static FILE_DIRS: &[&str] = &[
    "/home/user/documents",
    "/var/log",
    "/tmp",
    "/opt/data",
    "/usr/local/share",
    "/etc/config",
    "C:/Users/user/Documents",
    "/home/user/projects",
    "/data/exports",
    "/var/www/html",
];

// ── Vehicle ────────────────────────────────────────────────────────────

static VEHICLE_MAKES: &[&str] = &[
    "Toyota",
    "Honda",
    "Ford",
    "Chevrolet",
    "BMW",
    "Mercedes-Benz",
    "Audi",
    "Volkswagen",
    "Tesla",
    "Hyundai",
    "Kia",
    "Nissan",
    "Subaru",
    "Mazda",
    "Lexus",
    "Volvo",
    "Porsche",
    "Jeep",
    "Land Rover",
    "Jaguar",
];

static VEHICLE_MODELS: &[&str] = &[
    "Camry",
    "Civic",
    "F-150",
    "Silverado",
    "3 Series",
    "C-Class",
    "A4",
    "Golf",
    "Model 3",
    "Tucson",
    "Sportage",
    "Altima",
    "Outback",
    "CX-5",
    "RX",
    "XC90",
    "911",
    "Wrangler",
    "Range Rover",
    "F-Type",
];

// ── Medical ────────────────────────────────────────────────────────────

static BLOOD_TYPES: &[&str] = &["A+", "A-", "B+", "B-", "AB+", "AB-", "O+", "O-"];

// ── Company extras ─────────────────────────────────────────────────────

static INDUSTRIES: &[&str] = &[
    "Technology",
    "Healthcare",
    "Finance",
    "Education",
    "Manufacturing",
    "Retail",
    "Energy",
    "Transportation",
    "Telecommunications",
    "Agriculture",
    "Construction",
    "Entertainment",
    "Real Estate",
    "Hospitality",
    "Legal",
    "Consulting",
    "Pharmaceuticals",
    "Insurance",
    "Aerospace",
    "Automotive",
];

static CATCH_PHRASE_ADJECTIVES: &[&str] = &[
    "Adaptive",
    "Advanced",
    "Automated",
    "Balanced",
    "Centralized",
    "Compatible",
    "Configurable",
    "Cross-platform",
    "Decentralized",
    "Digitized",
    "Distributed",
    "Enhanced",
    "Ergonomic",
    "Exclusive",
    "Extended",
    "Focused",
    "Horizontal",
    "Innovative",
    "Integrated",
    "Intuitive",
    "Managed",
    "Multi-layered",
    "Networked",
    "Open-source",
    "Optimized",
    "Persistent",
    "Proactive",
    "Programmable",
    "Progressive",
    "Reactive",
    "Realigned",
    "Reduced",
    "Robust",
    "Seamless",
    "Secured",
    "Streamlined",
    "Switchable",
    "Synchronized",
    "Universal",
    "Upgradable",
    "Versatile",
    "Virtual",
];

static CATCH_PHRASE_DESCRIPTORS: &[&str] = &[
    "24/7",
    "actuating",
    "analyzing",
    "asymmetric",
    "asynchronous",
    "attitude-oriented",
    "background",
    "bandwidth-monitored",
    "bi-directional",
    "bottom-line",
    "client-driven",
    "client-server",
    "coherent",
    "cohesive",
    "composite",
    "context-sensitive",
    "contextually-based",
    "content-based",
    "dedicated",
    "demand-driven",
    "didactic",
    "directional",
    "discrete",
    "dynamic",
    "eco-centric",
    "empowering",
    "encompassing",
    "even-keeled",
    "executive",
    "explicit",
    "exuding",
    "fault-tolerant",
    "foreground",
    "fresh-thinking",
    "full-range",
    "global",
    "grid-enabled",
    "heuristic",
    "high-level",
    "holistic",
    "homogeneous",
    "human-resource",
    "hybrid",
    "impactful",
    "incremental",
    "intangible",
    "interactive",
    "intermediate",
    "leading edge",
    "local",
    "logistical",
    "maximized",
    "methodical",
    "mission-critical",
    "mobile",
    "modular",
    "motivating",
    "multi-state",
    "multi-tasking",
    "national",
    "needs-based",
    "neutral",
    "next generation",
    "non-volatile",
    "object-oriented",
    "optimal",
    "optimizing",
];

static CATCH_PHRASE_NOUNS: &[&str] = &[
    "ability",
    "access",
    "adapter",
    "algorithm",
    "alliance",
    "analyzer",
    "application",
    "approach",
    "architecture",
    "archive",
    "benchmark",
    "budgetary management",
    "capability",
    "capacity",
    "challenge",
    "circuit",
    "collaboration",
    "complexity",
    "concept",
    "conglomeration",
    "contingency",
    "core",
    "customer loyalty",
    "data-warehouse",
    "database",
    "definition",
    "emulation",
    "encoding",
    "encryption",
    "extranet",
    "firmware",
    "flexibility",
    "focus group",
    "forecast",
    "frame",
    "framework",
    "function",
    "graphic interface",
    "groupware",
    "hardware",
    "help-desk",
    "hierarchy",
    "hub",
    "implementation",
    "infrastructure",
    "initiative",
    "installation",
    "instruction set",
    "interface",
    "intranet",
    "knowledge base",
    "leverage",
    "local area network",
    "matrices",
    "methodology",
    "middleware",
    "migration",
    "model",
    "moderator",
    "monitoring",
    "moratorium",
];

static BS_VERBS: &[&str] = &[
    "implement",
    "utilize",
    "integrate",
    "streamline",
    "optimize",
    "evolve",
    "transform",
    "embrace",
    "enable",
    "orchestrate",
    "leverage",
    "reinvent",
    "aggregate",
    "architect",
    "benchmark",
    "brand",
    "cultivate",
    "deliver",
    "deploy",
    "disintermediate",
    "drive",
    "e-enable",
    "empower",
    "engage",
    "engineer",
];

static BS_ADJECTIVES: &[&str] = &[
    "clicks-and-mortar",
    "value-added",
    "vertical",
    "proactive",
    "robust",
    "revolutionary",
    "scalable",
    "leading-edge",
    "innovative",
    "intuitive",
    "strategic",
    "e-business",
    "mission-critical",
    "sticky",
    "one-to-one",
    "24/7",
    "end-to-end",
    "global",
    "B2B",
    "B2C",
    "granular",
    "frictionless",
    "virtual",
    "viral",
    "dynamic",
];

static BS_NOUNS: &[&str] = &[
    "synergies",
    "web-readiness",
    "paradigms",
    "markets",
    "partnerships",
    "infrastructures",
    "platforms",
    "initiatives",
    "channels",
    "eyeballs",
    "communities",
    "ROI",
    "solutions",
    "e-tailers",
    "e-services",
    "action-items",
    "portals",
    "niches",
    "technologies",
    "content",
    "supply-chains",
    "convergence",
    "relationships",
    "architectures",
    "interfaces",
];

// ---------------------------------------------------------------------------
// Helper
// ---------------------------------------------------------------------------

/// Pick a random element from a static slice using the given RNG.
fn pick<'a>(rng: &mut dyn Rng, list: &'a [&str]) -> &'a str {
    list[rng.next_u32() as usize % list.len()]
}

/// Generate a credit card number with valid Luhn checksum.
/// Prefixes: Visa (4), Mastercard (51-55), Amex (34/37).
fn generate_credit_card(rng: &mut dyn Rng) -> String {
    let choice = rng.next_u32() % 3;
    let (prefix, total_len) = match choice {
        0 => ("4", 16usize), // Visa
        1 => {
            let mc = 51 + rng.next_u32() % 5;
            return generate_cc_with_prefix(&mc.to_string(), 16, rng);
        }
        _ => {
            let amex = if rng.next_u32().is_multiple_of(2) {
                "34"
            } else {
                "37"
            };
            return generate_cc_with_prefix(amex, 15, rng);
        }
    };
    generate_cc_with_prefix(prefix, total_len, rng)
}

fn generate_cc_with_prefix(prefix: &str, total_len: usize, rng: &mut dyn Rng) -> String {
    let mut digits: Vec<u8> = prefix.bytes().map(|b| b - b'0').collect();
    // Fill random digits (leave last for check digit)
    while digits.len() < total_len - 1 {
        digits.push((rng.next_u32() % 10) as u8);
    }
    // Luhn check digit
    let check = luhn_check_digit(&digits);
    digits.push(check);
    digits.iter().map(|d| (b'0' + d) as char).collect()
}

fn luhn_check_digit(digits: &[u8]) -> u8 {
    let mut sum = 0u32;
    for (i, &d) in digits.iter().rev().enumerate() {
        let mut v = d as u32;
        if i % 2 == 0 {
            v *= 2;
            if v > 9 {
                v -= 9;
            }
        }
        sum += v;
    }
    ((10 - (sum % 10)) % 10) as u8
}

/// Generate a simplified IBAN with valid check digits (mod-97).
/// Only uses countries whose BBAN format is entirely numeric digits,
/// so the generated IBANs pass standard validators.
fn generate_iban(rng: &mut dyn Rng) -> String {
    // Countries with all-numeric BBANs (no embedded letters)
    let country = pick(rng, &["DE", "FR", "AT", "ES", "FI", "PT", "NO"]);
    let bban_len = match country {
        "DE" => 18, // 8 bank + 10 account
        "FR" => 23, // 5 bank + 5 branch + 11 account + 2 check
        "AT" => 16, // 5 bank + 11 account
        "ES" => 20, // 4 bank + 4 branch + 2 check + 10 account
        "FI" => 14, // 3 bank + 11 account (includes check)
        "PT" => 21, // 4 bank + 4 branch + 11 account + 2 check
        "NO" => 11, // 4 bank + 6 account + 1 check
        _ => 16,
    };
    let bban: String = (0..bban_len)
        .map(|_| (b'0' + (rng.next_u32() % 10) as u8) as char)
        .collect();
    // Compute check digits: move country + 00 to end, convert letters to numbers
    let check_str = format!("{}{}{:02}", bban, country_to_digits(country), 0);
    let remainder = mod97(&check_str);
    let check = 98 - remainder;
    format!("{country}{check:02}{bban}")
}

fn country_to_digits(code: &str) -> String {
    code.chars()
        .map(|c| ((c as u32) - ('A' as u32) + 10).to_string())
        .collect()
}

fn mod97(s: &str) -> u32 {
    let mut remainder = 0u32;
    for c in s.chars() {
        let digit = c.to_digit(10).unwrap_or(0);
        remainder = (remainder * 10 + digit) % 97;
    }
    remainder
}

/// Generate a 17-character VIN (no I, O, Q per spec).
fn generate_vin(rng: &mut dyn Rng) -> String {
    const VIN_CHARS: &[u8] = b"ABCDEFGHJKLMNPRSTUVWXYZ0123456789";
    let mut vin: Vec<u8> = (0..17)
        .map(|_| VIN_CHARS[rng.next_u32() as usize % VIN_CHARS.len()])
        .collect();
    // Position 9 is the check digit (simplified: random digit)
    vin[8] = b'0' + (rng.next_u32() % 10) as u8;
    String::from_utf8(vin).expect("VIN bytes are generated from ASCII characters only")
}

/// Generate a valid EAN-13 barcode with check digit.
fn generate_ean13(rng: &mut dyn Rng) -> String {
    let mut digits: Vec<u8> = (0..12).map(|_| (rng.next_u32() % 10) as u8).collect();
    let check = ean_check_digit(&digits);
    digits.push(check);
    digits.iter().map(|d| (b'0' + d) as char).collect()
}

/// Generate a valid ISBN-13 with 978/979 prefix and check digit.
fn generate_isbn13(rng: &mut dyn Rng) -> String {
    let prefix = if rng.next_u32().is_multiple_of(2) {
        [9, 7, 8]
    } else {
        [9, 7, 9]
    };
    let mut digits: Vec<u8> = prefix.to_vec();
    for _ in 0..9 {
        digits.push((rng.next_u32() % 10) as u8);
    }
    let check = ean_check_digit(&digits);
    digits.push(check);
    digits.iter().map(|d| (b'0' + d) as char).collect()
}

fn ean_check_digit(digits: &[u8]) -> u8 {
    let sum: u32 = digits
        .iter()
        .enumerate()
        .map(|(i, &d)| if i % 2 == 0 { d as u32 } else { d as u32 * 3 })
        .sum();
    ((10 - (sum % 10)) % 10) as u8
}

// ---------------------------------------------------------------------------
// Generator
// ---------------------------------------------------------------------------

/// Produce realistic synthetic strings for a given faker *category* (method).
///
/// Supports dotted provider names (e.g. `"internet.email"`) which are
/// normalized to bare method names (e.g. `"email"`).
///
/// **Supported categories:**
/// - Person: `first_name`, `last_name`, `full_name`/`name`, `username`, `prefix`, `suffix`
/// - Internet: `email`, `url`, `domain`, `ipv4`/`ip_address`, `ipv6`, `mac_address`, `user_agent`
/// - Address: `address`/`street_address`, `city`, `state`, `country`, `country_code`, `zip_code`
/// - Company: `company`, `industry`, `catch_phrase`, `bs`
/// - Finance: `credit_card`, `iban`, `bic`, `currency_code`
/// - Phone: `phone`
/// - Lorem: `word`, `sentence`, `paragraph`, `title`
/// - Datetime: `date`, `datetime`, `time`, `month`, `day_of_week`, `timezone`
/// - Color: `color`, `hex_color`
/// - File: `file_extension`, `mime_type`, `file_name`, `file_path`
/// - Geo: `latitude`, `longitude`, `coordinate`
/// - Vehicle: `license_plate`, `vin`, `vehicle_make`, `vehicle_model`
/// - Medical: `blood_type`
/// - Barcode: `ean13`, `isbn13`
/// - Product: `product_name`/`product`
/// - Other: `hex_string`
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
    args: Vec<crate::core::Value>,
    /// Whether a warning has already been emitted for an unknown category.
    warned: AtomicBool,
}

impl FakerGenerator {
    /// Create a new faker generator for the given *category* and *locale*.
    ///
    /// Dotted provider names (e.g. `"internet.email"`, `"finance.credit_card"`)
    /// are normalized to their bare method name (`"email"`, `"credit_card"`).
    pub fn new(category: String, locale: String, args: Vec<crate::core::Value>) -> Self {
        // Normalize dotted provider.method → method
        let normalized = if let Some((_provider, method)) = category.split_once('.') {
            method.to_string()
        } else {
            category
        };
        Self {
            category: normalized,
            locale,
            args,
            warned: AtomicBool::new(false),
        }
    }

    /// Parse date range from args, falling back to 2020-01-01..2024-12-31.
    fn date_range(&self) -> (i64, i64) {
        let default_start = days_from_epoch(2020, 1, 1);
        let default_end = days_from_epoch(2024, 12, 31);

        let parse_date = |v: &crate::core::Value| -> Option<i64> {
            if let crate::core::Value::String(s) = v {
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

        let start = self
            .args
            .first()
            .and_then(parse_date)
            .unwrap_or(default_start);
        let end = self.args.get(1).and_then(parse_date).unwrap_or(default_end);
        (start, end)
    }

    /// Generate a single value for the configured category.
    fn generate_one(&self, rng: &mut dyn Rng) -> String {
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
                    parts[0], parts[1], parts[2], parts[3], parts[4], parts[5], parts[6], parts[7]
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
                let len = self
                    .args
                    .first()
                    .and_then(|v| match v {
                        crate::core::Value::Int(n) if *n > 0 => Some((*n as usize).min(1024)),
                        _ => None,
                    })
                    .unwrap_or(32);
                let byte_count = len.div_ceil(2);
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
            // ─── Person ────────────────────────────────────────────────
            "prefix" | "name_prefix" => pick(rng, NAME_PREFIXES).to_string(),
            "suffix" | "name_suffix" => pick(rng, NAME_SUFFIXES).to_string(),
            // ─── Internet ──────────────────────────────────────────────
            "mac_address" | "mac" => {
                let mut octets = [0u8; 6];
                rng.fill_bytes(&mut octets);
                format!(
                    "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                    octets[0], octets[1], octets[2], octets[3], octets[4], octets[5]
                )
            }
            "user_agent" => pick(rng, USER_AGENTS).to_string(),
            // ─── Finance ───────────────────────────────────────────────
            "credit_card" | "credit_card_number" => generate_credit_card(rng),
            "iban" => generate_iban(rng),
            "bic" | "swift" => {
                // BIC/SWIFT: 8 or 11 chars — BANKCCLL(XXX)
                let bank: String = (0..4)
                    .map(|_| (b'A' + (rng.next_u32() % 26) as u8) as char)
                    .collect();
                let country = pick(rng, COUNTRY_CODES);
                let loc: String = (0..2)
                    .map(|_| {
                        let c = rng.next_u32() % 36;
                        if c < 26 {
                            (b'A' + c as u8) as char
                        } else {
                            (b'0' + (c - 26) as u8) as char
                        }
                    })
                    .collect();
                format!("{bank}{country}{loc}")
            }
            "currency_code" | "currency" => pick(rng, CURRENCY_CODES).to_string(),
            // ─── Geo ───────────────────────────────────────────────────
            "latitude" | "lat" => {
                let v = (rng.next_u32() as f64 / u32::MAX as f64) * 180.0 - 90.0;
                format!("{v:.6}")
            }
            "longitude" | "lon" | "lng" => {
                let v = (rng.next_u32() as f64 / u32::MAX as f64) * 360.0 - 180.0;
                format!("{v:.6}")
            }
            "coordinate" | "geo" => {
                let lat = (rng.next_u32() as f64 / u32::MAX as f64) * 180.0 - 90.0;
                let lon = (rng.next_u32() as f64 / u32::MAX as f64) * 360.0 - 180.0;
                format!("{lat:.6}, {lon:.6}")
            }
            // ─── Datetime ──────────────────────────────────────────────
            "month" => pick(rng, MONTHS).to_string(),
            "day_of_week" | "weekday" => pick(rng, WEEKDAYS).to_string(),
            "timezone" | "tz" => pick(rng, TIMEZONES).to_string(),
            "time" => {
                let h = rng.next_u32() % 24;
                let m = rng.next_u32() % 60;
                let s = rng.next_u32() % 60;
                format!("{h:02}:{m:02}:{s:02}")
            }
            // ─── File ──────────────────────────────────────────────────
            "file_extension" | "extension" => pick(rng, FILE_EXTENSIONS).to_string(),
            "mime_type" | "content_type" => pick(rng, MIME_TYPES).to_string(),
            "file_name" => {
                let w = pick(rng, WORDS);
                let ext = pick(rng, FILE_EXTENSIONS);
                format!("{w}.{ext}")
            }
            "file_path" => {
                let dir = pick(rng, FILE_DIRS);
                let w = pick(rng, WORDS);
                let ext = pick(rng, FILE_EXTENSIONS);
                format!("{dir}/{w}.{ext}")
            }
            // ─── Vehicle ───────────────────────────────────────────────
            "license_plate" | "plate" => {
                let letters: String = (0..3)
                    .map(|_| (b'A' + (rng.next_u32() % 26) as u8) as char)
                    .collect();
                let nums = rng.next_u32() % 10000;
                format!("{letters}-{nums:04}")
            }
            "vin" => generate_vin(rng),
            "vehicle_make" | "make" => pick(rng, VEHICLE_MAKES).to_string(),
            "vehicle_model" | "model" => pick(rng, VEHICLE_MODELS).to_string(),
            // ─── Medical ───────────────────────────────────────────────
            "blood_type" => pick(rng, BLOOD_TYPES).to_string(),
            // ─── Barcode ───────────────────────────────────────────────
            "ean13" => generate_ean13(rng),
            "isbn13" | "isbn" => generate_isbn13(rng),
            // ─── Company extras ────────────────────────────────────────
            "industry" => pick(rng, INDUSTRIES).to_string(),
            "catch_phrase" | "catchphrase" => {
                let adj = pick(rng, CATCH_PHRASE_ADJECTIVES);
                let descriptor = pick(rng, CATCH_PHRASE_DESCRIPTORS);
                let noun = pick(rng, CATCH_PHRASE_NOUNS);
                format!("{adj} {descriptor} {noun}")
            }
            "bs" | "buzzword" => {
                let verb = pick(rng, BS_VERBS);
                let adj = pick(rng, BS_ADJECTIVES);
                let noun = pick(rng, BS_NOUNS);
                format!("{verb} {adj} {noun}")
            }
            // ─── Address extras ────────────────────────────────────────
            "street_address" | "street" => {
                let house = 1 + rng.next_u32() % 9999;
                let name = pick(rng, STREET_NAMES);
                let suffix = pick(rng, STREET_SUFFIXES);
                format!("{house} {name} {suffix}")
            }
            "city_name" => pick(rng, CITIES).to_string(),
            "country_code" => pick(rng, COUNTRY_CODES).to_string(),
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
    fn generate(&self, rng: &mut dyn Rng, count: usize, _ctx: &GenContext) -> ArrayRef {
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
                        days * 86_400_000_000_000
                            + h * 3_600_000_000_000
                            + min * 60_000_000_000
                            + s * 1_000_000_000
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
                let values: Vec<String> = (0..count).map(|_| self.generate_one(rng)).collect();
                Arc::new(StringArray::from(
                    values.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                ))
            }
        }
    }

    fn output_type(&self) -> DataType {
        match self.category.as_str() {
            "datetime" | "timestamp" => DataType::Timestamp(TimeUnit::Nanosecond, None),
            "date" => DataType::Date32,
            _ => DataType::Utf8,
        }
    }
}

/// Convert a civil date to days since Unix epoch (1970-01-01).
fn days_from_epoch(year: i32, month: u32, day: u32) -> i64 {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let y = if month <= 2 {
        year as i64 - 1
    } else {
        year as i64
    };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let m = month;
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
    use rand::rngs::ChaCha8Rng;
    use std::collections::HashMap;

    fn make_ctx() -> GenContext<'static> {
        let map: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(HashMap::new()));
        GenContext::new(map, 0, 0, 1, "test")
    }

    fn r#gen(category: &str, count: usize, seed: u64) -> ArrayRef {
        let g = FakerGenerator::new(category.into(), "en_US".into(), vec![]);
        let ctx = make_ctx();
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        g.generate(&mut rng, count, &ctx)
    }

    fn gen_with_args(
        category: &str,
        args: Vec<crate::core::Value>,
        count: usize,
        seed: u64,
    ) -> ArrayRef {
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
        let arr = r#gen("first_name", 50, 1);
        assert_eq!(arr.len(), 50);
        for v in strings(&arr) {
            assert!(!v.is_empty(), "first_name produced empty string");
        }
    }

    #[test]
    fn last_name_produces_nonempty_strings() {
        let arr = r#gen("last_name", 50, 2);
        assert_eq!(arr.len(), 50);
        for v in strings(&arr) {
            assert!(!v.is_empty());
        }
    }

    #[test]
    fn full_name_contains_space() {
        let arr = r#gen("full_name", 50, 3);
        for v in strings(&arr) {
            assert!(v.contains(' '), "full_name missing space: {v}");
        }
    }

    #[test]
    fn username_format() {
        let arr = r#gen("username", 100, 4);
        for v in strings(&arr) {
            assert!(v.contains('_'), "username missing underscore: {v}");
            assert_eq!(v, v.to_lowercase(), "username not lowercase: {v}");
        }
    }

    #[test]
    fn email_contains_at() {
        let arr = r#gen("email", 100, 5);
        for v in strings(&arr) {
            assert!(v.contains('@'), "email missing @: {v}");
            assert!(v.contains('.'), "email missing dot: {v}");
        }
    }

    #[test]
    fn phone_format() {
        let arr = r#gen("phone", 100, 6);
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
        let arr = r#gen("sentence", 50, 7);
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
        let arr = r#gen("word", 50, 8);
        for v in strings(&arr) {
            assert!(!v.is_empty());
        }
    }

    #[test]
    fn address_has_number_and_street() {
        let arr = r#gen("address", 50, 9);
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
        let arr = r#gen("city", 50, 10);
        for v in strings(&arr) {
            assert!(!v.is_empty());
        }
    }

    #[test]
    fn company_nonempty() {
        let arr = r#gen("company", 50, 11);
        for v in strings(&arr) {
            assert!(v.contains(' '), "company missing space: {v}");
        }
    }

    #[test]
    fn unknown_method_does_not_panic() {
        let arr = r#gen("nonexistent_method", 10, 12);
        assert_eq!(arr.len(), 10);
        for v in strings(&arr) {
            assert_eq!(v, "nonexistent_method");
        }
    }

    #[test]
    fn deterministic_with_same_seed() {
        let a = r#gen("email", 20, 42);
        let b = r#gen("email", 20, 42);
        let va = strings(&a);
        let vb = strings(&b);
        assert_eq!(va, vb, "same seed must produce same output");
    }

    #[test]
    fn correct_count() {
        for count in [0, 1, 5, 100] {
            let arr = r#gen("first_name", count, 99);
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
        let arr = r#gen("name", 20, 55);
        for v in strings(&arr) {
            assert!(v.contains(' '), "name alias missing space: {v}");
        }
    }

    #[test]
    fn state_produces_known_state() {
        let arr = r#gen("state", 50, 20);
        for v in strings(&arr) {
            assert!(
                super::US_STATES.contains(&v.as_str()),
                "state should be from US_STATES list: {v}"
            );
        }
    }

    #[test]
    fn country_produces_known_country() {
        let arr = r#gen("country", 50, 21);
        for v in strings(&arr) {
            assert!(
                super::COUNTRIES.contains(&v.as_str()),
                "country should be from COUNTRIES list: {v}"
            );
        }
    }

    #[test]
    fn zip_code_five_digits() {
        let arr = r#gen("zip_code", 100, 22);
        for v in strings(&arr) {
            assert_eq!(v.len(), 5, "zip_code should be 5 chars: {v}");
            assert!(
                v.chars().all(|c| c.is_ascii_digit()),
                "zip_code should be digits: {v}"
            );
        }
    }

    #[test]
    fn zip_code_aliases() {
        // All aliases should produce 5-digit codes
        for alias in &["zip_code", "zipcode", "postal_code"] {
            let arr = r#gen(alias, 10, 23);
            for v in strings(&arr) {
                assert_eq!(v.len(), 5, "{alias} should produce 5-digit code: {v}");
            }
        }
    }

    #[test]
    fn url_format() {
        let arr = r#gen("url", 50, 24);
        for v in strings(&arr) {
            assert!(
                v.starts_with("https://"),
                "url should start with https://: {v}"
            );
            assert!(v.contains('.'), "url should contain a dot: {v}");
        }
    }

    #[test]
    fn domain_format() {
        let arr = r#gen("domain", 50, 25);
        for v in strings(&arr) {
            assert!(
                !v.starts_with("https://"),
                "domain should not have scheme: {v}"
            );
            assert!(v.contains('.'), "domain should contain a dot: {v}");
        }
    }

    #[test]
    fn ipv4_format() {
        let arr = r#gen("ipv4", 50, 26);
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
        let arr = r#gen("ip_address", 10, 27);
        for v in strings(&arr) {
            assert_eq!(
                v.split('.').count(),
                4,
                "ip_address should produce ipv4: {v}"
            );
        }
    }

    #[test]
    fn ipv6_format() {
        let arr = r#gen("ipv6", 50, 28);
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
        let arr = r#gen("color", 50, 29);
        for v in strings(&arr) {
            assert!(
                super::COLORS.contains(&v.as_str()),
                "color should be from COLORS list: {v}"
            );
        }
    }

    #[test]
    fn hex_color_format() {
        let arr = r#gen("hex_color", 50, 30);
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
        let arr = r#gen("paragraph", 20, 31);
        for v in strings(&arr) {
            assert!(v.ends_with('.'), "paragraph should end with period: {v}");
            let sentence_count = v.matches('.').count();
            assert!(
                sentence_count >= 2,
                "paragraph should have >=2 sentences: {v}"
            );
        }
    }

    #[test]
    fn title_title_case() {
        let arr = r#gen("title", 50, 32);
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
        let arr = r#gen("hex_string", 50, 40);
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
        let arr = gen_with_args("hex_string", vec![crate::core::Value::Int(40)], 50, 41);
        for v in strings(&arr) {
            assert_eq!(
                v.len(),
                40,
                "hex_string with arg 40 should be 40 chars: {v}"
            );
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

    // ─── Tests for newly added faker methods ───────────────────────────

    #[test]
    fn mac_address_format() {
        let arr = r#gen("mac_address", 50, 100);
        for v in strings(&arr) {
            let parts: Vec<&str> = v.split(':').collect();
            assert_eq!(parts.len(), 6, "MAC should have 6 octets: {v}");
            for p in &parts {
                assert_eq!(p.len(), 2, "MAC octet should be 2 hex chars: {v}");
                assert!(p.chars().all(|c| c.is_ascii_hexdigit()), "hex octet: {v}");
            }
        }
    }

    #[test]
    fn credit_card_luhn_valid() {
        let arr = r#gen("credit_card", 100, 101);
        for v in strings(&arr) {
            let len = v.len();
            assert!(
                len == 15 || len == 16,
                "credit card should be 15 or 16 digits: {v}"
            );
            assert!(v.chars().all(|c| c.is_ascii_digit()), "all digits: {v}");
            // Verify Luhn checksum
            let digits: Vec<u8> = v.bytes().map(|b| b - b'0').collect();
            let mut sum = 0u32;
            for (i, &d) in digits.iter().rev().enumerate() {
                let mut val = d as u32;
                if i % 2 == 1 {
                    val *= 2;
                    if val > 9 {
                        val -= 9;
                    }
                }
                sum += val;
            }
            assert_eq!(sum % 10, 0, "Luhn check failed for {v}");
        }
    }

    #[test]
    fn iban_format() {
        let arr = r#gen("iban", 50, 102);
        for v in strings(&arr) {
            assert!(v.len() >= 15, "IBAN should be >= 15 chars: {v}");
            // First 2 chars are country code (uppercase letters)
            assert!(
                v[..2].chars().all(|c| c.is_ascii_uppercase()),
                "IBAN country code: {v}"
            );
            // Next 2 chars are check digits
            assert!(
                v[2..4].chars().all(|c| c.is_ascii_digit()),
                "IBAN check digits: {v}"
            );
        }
    }

    #[test]
    fn bic_format() {
        let arr = r#gen("bic", 50, 103);
        for v in strings(&arr) {
            assert_eq!(v.len(), 8, "BIC should be 8 chars: {v}");
        }
    }

    #[test]
    fn currency_code_valid() {
        let arr = r#gen("currency_code", 50, 104);
        for v in strings(&arr) {
            assert_eq!(v.len(), 3, "currency code should be 3 chars: {v}");
            assert!(
                v.chars().all(|c| c.is_ascii_uppercase()),
                "currency code should be uppercase: {v}"
            );
        }
    }

    #[test]
    fn latitude_in_range() {
        let arr = r#gen("latitude", 50, 105);
        for v in strings(&arr) {
            let lat: f64 = v.parse().unwrap();
            assert!(
                (-90.0..=90.0).contains(&lat),
                "latitude out of range: {lat}"
            );
        }
    }

    #[test]
    fn longitude_in_range() {
        let arr = r#gen("longitude", 50, 106);
        for v in strings(&arr) {
            let lon: f64 = v.parse().unwrap();
            assert!(
                (-180.0..=180.0).contains(&lon),
                "longitude out of range: {lon}"
            );
        }
    }

    #[test]
    fn vin_format() {
        let arr = r#gen("vin", 50, 107);
        for v in strings(&arr) {
            assert_eq!(v.len(), 17, "VIN should be 17 chars: {v}");
            assert!(
                !v.contains('I') && !v.contains('O') && !v.contains('Q'),
                "VIN should not contain I/O/Q: {v}"
            );
        }
    }

    #[test]
    fn ean13_valid_checksum() {
        let arr = r#gen("ean13", 50, 108);
        for v in strings(&arr) {
            assert_eq!(v.len(), 13, "EAN-13 should be 13 digits: {v}");
            assert!(v.chars().all(|c| c.is_ascii_digit()), "all digits: {v}");
            // Verify EAN check digit
            let digits: Vec<u8> = v.bytes().map(|b| b - b'0').collect();
            let sum: u32 = digits
                .iter()
                .enumerate()
                .map(|(i, &d)| if i % 2 == 0 { d as u32 } else { d as u32 * 3 })
                .sum();
            assert_eq!(sum % 10, 0, "EAN-13 check digit failed for {v}");
        }
    }

    #[test]
    fn isbn13_prefix() {
        let arr = r#gen("isbn13", 50, 109);
        for v in strings(&arr) {
            assert_eq!(v.len(), 13, "ISBN-13 should be 13 digits: {v}");
            assert!(
                v.starts_with("978") || v.starts_with("979"),
                "ISBN-13 should start with 978/979: {v}"
            );
        }
    }

    #[test]
    fn blood_type_valid() {
        let arr = r#gen("blood_type", 50, 110);
        for v in strings(&arr) {
            assert!(
                super::BLOOD_TYPES.contains(&v.as_str()),
                "blood_type should be valid: {v}"
            );
        }
    }

    #[test]
    fn license_plate_format() {
        let arr = r#gen("license_plate", 50, 111);
        for v in strings(&arr) {
            assert!(v.contains('-'), "license plate should contain dash: {v}");
        }
    }

    #[test]
    fn month_valid() {
        let arr = r#gen("month", 50, 112);
        for v in strings(&arr) {
            assert!(
                super::MONTHS.contains(&v.as_str()),
                "month should be valid: {v}"
            );
        }
    }

    #[test]
    fn day_of_week_valid() {
        let arr = r#gen("day_of_week", 50, 113);
        for v in strings(&arr) {
            assert!(
                super::WEEKDAYS.contains(&v.as_str()),
                "weekday should be valid: {v}"
            );
        }
    }

    #[test]
    fn file_name_has_extension() {
        let arr = r#gen("file_name", 50, 114);
        for v in strings(&arr) {
            assert!(v.contains('.'), "file_name should have extension: {v}");
        }
    }

    #[test]
    fn dotted_provider_name_normalized() {
        // Dotted names like "internet.email" should work the same as "email"
        let a = r#gen("internet.email", 20, 42);
        let b = r#gen("email", 20, 42);
        let a_strs = strings(&a);
        let b_strs = strings(&b);
        assert_eq!(a_strs, b_strs, "dotted name should produce same output");
    }

    #[test]
    fn time_format() {
        let arr = r#gen("time", 50, 115);
        for v in strings(&arr) {
            let parts: Vec<&str> = v.split(':').collect();
            assert_eq!(parts.len(), 3, "time should have HH:MM:SS: {v}");
        }
    }

    #[test]
    fn industry_from_list() {
        let arr = r#gen("industry", 50, 116);
        for v in strings(&arr) {
            assert!(
                super::INDUSTRIES.contains(&v.as_str()),
                "industry should be from list: {v}"
            );
        }
    }

    #[test]
    fn user_agent_nonempty() {
        let arr = r#gen("user_agent", 50, 117);
        for v in strings(&arr) {
            assert!(
                v.contains("Mozilla") || v.len() > 10,
                "user_agent should look real: {v}"
            );
        }
    }
}
