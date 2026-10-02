//! The vocabulary: plain generic words search treats specially (plan
//! §3.2, decisions 8 and 12b).  Each word has a kind — an object, a scene,
//! or one of the subject kinds (people, fursuits, real animals) — a
//! display form (the plural the lists show) and the prompt its hidden tag
//! is derived from.  Nothing personal lives here: the words are the
//! generic things an event, home, travel and nature library holds.
//!
//! **Kinds never mix** (decision 12, UI 3): the *Things* list shows
//! objects and scenes only; people, fursuits and real animals get pages
//! of their own in later phases, and a real animal's results never carry
//! fursuits.  [`Kind::is_subject`] is the one place that says which kinds
//! are kept out, and the tests at the end hold it.

/// What a vocabulary word names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A thing: a camera, a car, food, a balloon.
    Object,
    /// A place or a scene: a stage, a beach, fireworks, a sunset.
    Scene,
    /// People (phase 3 gives them their own pages).
    Person,
    /// Fursuits (phase 4).
    Fursuit,
    /// Real animals: pets (phase 5).  To the models, many of these words
    /// mean fursuits in this library ([`Word::fursuit_lookalike`]).
    Animal,
}

impl Kind {
    /// People, fursuits and animals: never in *Things*, never combined
    /// with one another.
    pub fn is_subject(self) -> bool {
        matches!(self, Kind::Person | Kind::Fursuit | Kind::Animal)
    }
}

/// One word of the vocabulary.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Word {
    /// The word as a query names it: lower case, singular ("camera",
    /// "night sky").
    pub word: &'static str,
    /// The form the lists show and completions insert ("cameras",
    /// "fireworks", "food").
    pub plural: &'static str,
    pub kind: Kind,
    /// Fursuits are taken for it: a photo carries its tag only where the
    /// word beats "fursuit" (the measured contrast), and for a real
    /// animal (decision 12b) a query naming it, and not naming fursuits,
    /// leaves fursuits out by default.  Set on the animals and on the
    /// things the sample confused with fursuits (a stuffed animal: 139 of
    /// 148 tagged were fursuits before the contrast, every one left after
    /// it a real plush toy).
    pub fursuit_lookalike: bool,
    /// The match probability ([`super::query::match_probability`]) a
    /// photo needs to carry the word as a hidden tag.  Calibrated on the
    /// phase 0 sample (see `tags.rs`); `None` is the vocabulary's default.
    pub threshold: Option<f32>,
}

impl Word {
    /// The text its tag vector is encoded from (the query template).
    pub fn prompt(&self) -> String {
        super::query::prompt(self.word)
    }
}

const fn w(word: &'static str, plural: &'static str, kind: Kind) -> Word {
    Word { word, plural, kind, fursuit_lookalike: false, threshold: None }
}

/// A real animal fursuits are taken for.
const fn a(word: &'static str, plural: &'static str) -> Word {
    Word { word, plural, kind: Kind::Animal, fursuit_lookalike: true, threshold: None }
}

/// A real animal nobody takes a fursuit for.
const fn a0(word: &'static str, plural: &'static str) -> Word {
    Word { word, plural, kind: Kind::Animal, fursuit_lookalike: false, threshold: None }
}

/// The vocabulary.  Order is the tag index order (`tags.rs` stores tags
/// by position), so append rather than reorder when adding words.
pub const VOCABULARY: &[Word] = &[
    // Objects.
    w("camera", "cameras", Kind::Object),
    w("lens", "lenses", Kind::Object),
    w("phone", "phones", Kind::Object),
    w("laptop", "laptops", Kind::Object),
    w("screen", "screens", Kind::Object),
    w("television", "televisions", Kind::Object),
    w("car", "cars", Kind::Object),
    w("truck", "trucks", Kind::Object),
    w("motorcycle", "motorcycles", Kind::Object),
    w("bicycle", "bicycles", Kind::Object),
    w("bus", "buses", Kind::Object),
    w("train", "trains", Kind::Object),
    w("airplane", "airplanes", Kind::Object),
    w("boat", "boats", Kind::Object),
    w("food", "food", Kind::Object),
    w("drink", "drinks", Kind::Object),
    w("cake", "cakes", Kind::Object),
    w("pizza", "pizza", Kind::Object),
    w("coffee", "coffee", Kind::Object),
    w("beer", "beer", Kind::Object),
    w("ice cream", "ice cream", Kind::Object),
    w("sign", "signs", Kind::Object),
    w("poster", "posters", Kind::Object),
    w("book", "books", Kind::Object),
    w("flower", "flowers", Kind::Object),
    w("plant", "plants", Kind::Object),
    w("tree", "trees", Kind::Object),
    w("christmas tree", "christmas trees", Kind::Object),
    w("toy", "toys", Kind::Object),
    Word { word: "stuffed animal", plural: "stuffed animals", kind: Kind::Object, fursuit_lookalike: true, threshold: None },
    w("guitar", "guitars", Kind::Object),
    w("piano", "pianos", Kind::Object),
    w("drum", "drums", Kind::Object),
    w("microphone", "microphones", Kind::Object),
    w("flag", "flags", Kind::Object),
    w("balloon", "balloons", Kind::Object),
    w("hat", "hats", Kind::Object),
    w("glasses", "glasses", Kind::Object),
    w("mask", "masks", Kind::Object),
    w("badge", "badges", Kind::Object),
    w("umbrella", "umbrellas", Kind::Object),
    w("bag", "bags", Kind::Object),
    w("candle", "candles", Kind::Object),
    w("lantern", "lanterns", Kind::Object),
    w("statue", "statues", Kind::Object),
    w("fountain", "fountains", Kind::Object),
    w("tent", "tents", Kind::Object),
    w("sword", "swords", Kind::Object),
    w("gift", "gifts", Kind::Object),
    w("clock", "clocks", Kind::Object),
    // Scenes.
    w("stage", "stages", Kind::Scene),
    w("dance floor", "dance floors", Kind::Scene),
    w("party", "parties", Kind::Scene),
    w("concert", "concerts", Kind::Scene),
    w("convention hall", "convention halls", Kind::Scene),
    w("hotel lobby", "hotel lobbies", Kind::Scene),
    w("hotel room", "hotel rooms", Kind::Scene),
    w("restaurant", "restaurants", Kind::Scene),
    w("kitchen", "kitchens", Kind::Scene),
    w("living room", "living rooms", Kind::Scene),
    w("bedroom", "bedrooms", Kind::Scene),
    w("office", "offices", Kind::Scene),
    w("hallway", "hallways", Kind::Scene),
    w("elevator", "elevators", Kind::Scene),
    w("street", "streets", Kind::Scene),
    w("parking lot", "parking lots", Kind::Scene),
    w("building", "buildings", Kind::Scene),
    w("castle", "castles", Kind::Scene),
    w("church", "churches", Kind::Scene),
    w("bridge", "bridges", Kind::Scene),
    w("museum", "museums", Kind::Scene),
    w("airport", "airports", Kind::Scene),
    w("stadium", "stadiums", Kind::Scene),
    w("swimming pool", "swimming pools", Kind::Scene),
    w("park", "parks", Kind::Scene),
    w("garden", "gardens", Kind::Scene),
    w("forest", "forests", Kind::Scene),
    w("field", "fields", Kind::Scene),
    w("mountain", "mountains", Kind::Scene),
    w("snow", "snow", Kind::Scene),
    w("beach", "beaches", Kind::Scene),
    w("lake", "lakes", Kind::Scene),
    w("river", "rivers", Kind::Scene),
    w("waterfall", "waterfalls", Kind::Scene),
    w("sky", "sky", Kind::Scene),
    w("sunset", "sunsets", Kind::Scene),
    w("night sky", "night sky", Kind::Scene),
    w("fireworks", "fireworks", Kind::Scene),
    w("campfire", "campfires", Kind::Scene),
    w("city skyline", "city skylines", Kind::Scene),
    w("rain", "rain", Kind::Scene),
    // People.
    w("person", "people", Kind::Person),
    w("group of people", "groups of people", Kind::Person),
    w("crowd", "crowds", Kind::Person),
    w("child", "children", Kind::Person),
    w("baby", "babies", Kind::Person),
    w("selfie", "selfies", Kind::Person),
    w("portrait", "portraits", Kind::Person),
    Word { word: "cosplay", plural: "cosplay", kind: Kind::Person, fursuit_lookalike: true, threshold: None },
    // Fursuits.
    w("fursuit", "fursuits", Kind::Fursuit),
    w("mascot costume", "mascot costumes", Kind::Fursuit),
    // Real animals.
    a("dog", "dogs"),
    a("puppy", "puppies"),
    a("husky", "huskies"),
    a("wolf", "wolves"),
    a("fox", "foxes"),
    a("coyote", "coyotes"),
    a("jackal", "jackals"),
    a("cat", "cats"),
    a("kitten", "kittens"),
    a("lion", "lions"),
    a("tiger", "tigers"),
    a("leopard", "leopards"),
    a("cheetah", "cheetahs"),
    a("lynx", "lynxes"),
    a("bird", "birds"),
    a("owl", "owls"),
    a("eagle", "eagles"),
    a("hawk", "hawks"),
    a("parrot", "parrots"),
    a("crow", "crows"),
    a("raven", "ravens"),
    a("duck", "ducks"),
    a("goose", "geese"),
    a("chicken", "chickens"),
    a("penguin", "penguins"),
    a("horse", "horses"),
    a("pony", "ponies"),
    a("deer", "deer"),
    a("elk", "elk"),
    a("moose", "moose"),
    a("reindeer", "reindeer"),
    a("rabbit", "rabbits"),
    a("bunny", "bunnies"),
    a("hare", "hares"),
    a("bear", "bears"),
    a("panda", "pandas"),
    a("raccoon", "raccoons"),
    a("otter", "otters"),
    a("badger", "badgers"),
    a("skunk", "skunks"),
    a("hyena", "hyenas"),
    a("dragon", "dragons"),
    a("lizard", "lizards"),
    a("snake", "snakes"),
    a("crocodile", "crocodiles"),
    a("alligator", "alligators"),
    a("shark", "sharks"),
    a("dolphin", "dolphins"),
    a("whale", "whales"),
    a("fish", "fish"),
    a("cow", "cows"),
    a("bull", "bulls"),
    a("goat", "goats"),
    a("sheep", "sheep"),
    a("ram", "rams"),
    a("pig", "pigs"),
    a("boar", "boars"),
    a("mouse", "mice"),
    a("rat", "rats"),
    a("squirrel", "squirrels"),
    a("bat", "bats"),
    a("monkey", "monkeys"),
    a("ape", "apes"),
    a("gorilla", "gorillas"),
    a("kangaroo", "kangaroos"),
    a("koala", "koalas"),
    a("sloth", "sloths"),
    a("frog", "frogs"),
    a("turtle", "turtles"),
    a("ferret", "ferrets"),
    a("weasel", "weasels"),
    a("mink", "minks"),
    a("hamster", "hamsters"),
    a("zebra", "zebras"),
    a("giraffe", "giraffes"),
    a("elephant", "elephants"),
    a("rhino", "rhinos"),
    a("hippo", "hippos"),
    a("camel", "camels"),
    a("llama", "llamas"),
    a("alpaca", "alpacas"),
    a("donkey", "donkeys"),
    a("mule", "mules"),
    a("unicorn", "unicorns"),
    a("griffin", "griffins"),
    a("animal", "animals"),
    a("pet", "pets"),
    a0("insect", "insects"),
    a0("butterfly", "butterflies"),
    a0("bee", "bees"),
    a0("spider", "spiders"),
    a0("snail", "snails"),
    a0("crab", "crabs"),
    a0("jellyfish", "jellyfish"),
];

/// Words that mean the query wants fursuits (so the contrast is off).
pub const FURSUIT_WORDS: &[&str] = &["fursuit", "fursuits", "fursuiter", "fursuiters", "suit", "suits", "costume", "costumes", "mascot", "mascots", "furry", "furries"];

/// The vocabulary word at `index`.
pub fn word(index: usize) -> &'static Word {
    &VOCABULARY[index]
}

/// The vocabulary's words of the kinds *Things* shows: objects and
/// scenes, never a subject kind.
pub fn things() -> impl Iterator<Item = (usize, &'static Word)> {
    VOCABULARY.iter().enumerate().filter(|(_, w)| !w.kind.is_subject())
}

/// A token in lower case, singular: "cameras" → "camera", "parties" →
/// "party", "buses" → "bus", "lenses" → "lens"; a word's own plural
/// (`"mice"`, `"geese"`) maps back through the vocabulary.
pub fn singular(token: &str) -> String {
    let t = token.to_lowercase();
    if VOCABULARY.iter().any(|w| w.word == t) {
        return t;
    }
    if let Some(w) = VOCABULARY.iter().find(|w| w.plural == t) {
        return w.word.to_string();
    }
    if let Some(s) = t.strip_suffix("ies") {
        return format!("{s}y");
    }
    for suffix in ["ches", "shes", "sses", "xes", "zes"] {
        if let Some(s) = t.strip_suffix(suffix) {
            return format!("{s}{}", &suffix[..suffix.len() - 2]);
        }
    }
    if let Some(s) = t.strip_suffix("ses") {
        return format!("{s}s");
    }
    if let Some(s) = t.strip_suffix('s').filter(|s| !s.ends_with('s') && s.len() >= 2) {
        return s.to_string();
    }
    t
}

/// The vocabulary index of a word, by its singular, its plural or its
/// own text, whatever the case.
pub fn find(text: &str) -> Option<usize> {
    let s = singular(text.trim());
    VOCABULARY.iter().position(|w| w.word == s || w.plural == s)
}

/// The query's words, lower case and singular, in order.
pub fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '\'').filter(|w| !w.is_empty()).map(singular).collect()
}

/// Every vocabulary word `text` names (a multi-word entry needs its words
/// in a row), by index, in the order found, each once.
pub fn words_in(text: &str) -> Vec<usize> {
    let toks = tokens(text);
    // (position in the query, vocabulary index)
    let mut found: Vec<(usize, usize)> = Vec::new();
    for (i, w) in VOCABULARY.iter().enumerate() {
        let parts: Vec<String> = w.word.split(' ').map(singular).collect();
        if parts.is_empty() || parts.len() > toks.len() {
            continue;
        }
        if let Some(at) = toks.windows(parts.len()).position(|win| win == parts.as_slice()) {
            found.push((at, i));
        }
    }
    found.sort();
    let mut out: Vec<usize> = found.into_iter().map(|(_, i)| i).collect();
    // A longer entry hides the shorter ones it contains ("christmas tree"
    // isn't also "tree"; "night sky" isn't "sky").
    let longer: Vec<usize> = out.iter().copied().filter(|&i| VOCABULARY[i].word.contains(' ')).collect();
    out.retain(|&i| {
        let w = VOCABULARY[i].word;
        !longer.iter().any(|&l| l != i && VOCABULARY[l].word.split(' ').any(|p| p == w))
    });
    out
}

/// The query names fursuits on purpose.
pub fn names_fursuits(text: &str) -> bool {
    let raw: Vec<String> = text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect();
    raw.iter().any(|w| FURSUIT_WORDS.contains(&w.as_str())) || words_in(text).iter().any(|&i| VOCABULARY[i].kind == Kind::Fursuit)
}

/// The real animal the query names (the first animal word fursuits are
/// taken for), when it doesn't name fursuits: decision 12b's contrast
/// applies.
pub fn animal_in(text: &str) -> Option<&'static Word> {
    if names_fursuits(text) {
        return None;
    }
    words_in(text).into_iter().map(|i| &VOCABULARY[i]).find(|w| w.fursuit_lookalike && w.kind == Kind::Animal)
}

/// The query names a real animal and doesn't name fursuits (the
/// evaluation harness).
#[cfg(test)]
pub fn is_animal_query(text: &str) -> bool {
    animal_in(text).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn animal_queries_are_told_from_fursuit_queries() {
        assert!(is_animal_query("dog"));
        assert!(is_animal_query("two dogs on a beach"));
        assert!(is_animal_query("puppies"));
        assert!(is_animal_query("Bird"));
        assert!(!is_animal_query("dog fursuit"));
        assert!(!is_animal_query("fursuits"));
        assert!(!is_animal_query("camera"));
        assert!(!is_animal_query("fireworks -crowd"));
        assert!(!is_animal_query("butterfly"), "nobody takes a fursuit for a butterfly");
        assert_eq!(animal_in("a cat and a dog").map(|w| w.word), Some("cat"));
        assert_eq!(animal_in("dog mascot"), None);
    }

    #[test]
    fn words_are_found_singular_or_plural_and_longer_entries_win() {
        assert_eq!(singular("Cameras"), "camera");
        assert_eq!(singular("parties"), "party");
        assert_eq!(singular("buses"), "bus");
        assert_eq!(singular("lenses"), "lens");
        assert_eq!(singular("churches"), "church");
        assert_eq!(singular("mice"), "mouse");
        assert_eq!(singular("geese"), "goose");
        assert_eq!(singular("glasses"), "glasses");
        assert_eq!(singular("food"), "food");
        assert_eq!(singular("sky"), "sky");
        let names = |t: &str| words_in(t).into_iter().map(|i| VOCABULARY[i].word).collect::<Vec<_>>();
        assert_eq!(names("red cars at night"), ["car"]);
        assert_eq!(names("christmas tree"), ["christmas tree"]);
        assert_eq!(names("the night sky over the beach"), ["night sky", "beach"], "in the query's order");
        assert_eq!(names("fireworks -crowd"), ["fireworks", "crowd"]);
        assert!(names("zebra crossing").contains(&"zebra"));
        assert!(names("nothing here").is_empty());
        assert_eq!(find("Fireworks").map(|i| VOCABULARY[i].word), Some("fireworks"));
        assert_eq!(find("people").map(|i| VOCABULARY[i].word), Some("person"));
        assert_eq!(find("xylophone"), None);
    }

    /// Kinds never mix: the *Things* kinds are objects and scenes only,
    /// every word is unique, and every fursuit lookalike is an animal.
    #[test]
    fn the_vocabulary_is_sound_and_things_hold_no_subject_kinds() {
        let mut seen = HashSet::new();
        for w in VOCABULARY {
            assert!(seen.insert(w.word), "{} twice", w.word);
            assert_eq!(w.word, w.word.to_lowercase().trim());
            assert!(!w.word.is_empty() && !w.plural.is_empty());
            assert!(!w.fursuit_lookalike || w.kind != Kind::Fursuit, "{}", w.word);
            assert!(w.threshold.is_none_or(|t| t > 0.0 && t < 1.0), "{}", w.word);
            assert_eq!(w.prompt(), format!("a photo of a {}.", w.word));
        }
        for (_, w) in things() {
            assert!(!w.kind.is_subject(), "{} is in Things", w.word);
        }
        assert!(things().count() > 50);
        assert!(VOCABULARY.iter().any(|w| w.kind == Kind::Person));
        assert!(VOCABULARY.iter().any(|w| w.kind == Kind::Fursuit));
        assert!(VOCABULARY.iter().filter(|w| w.fursuit_lookalike).count() > 50);
        assert!(Kind::Person.is_subject() && Kind::Fursuit.is_subject() && Kind::Animal.is_subject());
        assert!(!Kind::Object.is_subject() && !Kind::Scene.is_subject());
    }
}
