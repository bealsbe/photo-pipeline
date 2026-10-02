//! Words search treats specially.  Phase 1 needs only the animal words
//! (decision 12b): to the models, "dog" means canine fursuits in this
//! library, so a query naming a real animal without naming fursuits leaves
//! fursuits out by default (the contrast, phase 2 builds the section).
//! Generic words only: nothing personal lives here.

/// Real animals that fursuits are taken for.  Lower case, singular; a
/// query word matches with or without a plural "s".
pub const ANIMAL_WORDS: &[&str] = &[
    "dog", "puppy", "husky", "wolf", "fox", "coyote", "jackal", "cat", "kitten", "lion", "tiger", "leopard", "cheetah",
    "lynx", "bird", "owl", "eagle", "hawk", "parrot", "crow", "raven", "duck", "goose", "chicken", "penguin", "horse",
    "pony", "deer", "elk", "moose", "reindeer", "rabbit", "bunny", "hare", "bear", "panda", "raccoon", "otter", "badger",
    "skunk", "hyena", "dragon", "lizard", "snake", "crocodile", "alligator", "shark", "dolphin", "whale", "fish", "cow",
    "bull", "goat", "sheep", "ram", "pig", "boar", "mouse", "rat", "squirrel", "bat", "monkey", "ape", "gorilla", "kangaroo",
    "koala", "sloth", "frog", "turtle", "ferret", "weasel", "mink", "hamster", "zebra", "giraffe", "elephant", "rhino",
    "hippo", "camel", "llama", "alpaca", "donkey", "mule", "unicorn", "griffin", "animal", "pet",
];

/// Words that mean the query wants fursuits (so the contrast is off).
pub const FURSUIT_WORDS: &[&str] = &["fursuit", "fursuits", "fursuiter", "fursuiters", "suit", "suits", "costume", "costumes", "mascot", "mascots", "furry", "furries"];

/// The query names a real animal and doesn't name fursuits.
pub fn is_animal_query(text: &str) -> bool {
    let words: Vec<String> = text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(|w| w.to_lowercase()).collect();
    if words.iter().any(|w| FURSUIT_WORDS.contains(&w.as_str())) {
        return false;
    }
    words.iter().any(|w| {
        let singular = w.strip_suffix("ies").map(|s| format!("{s}y")).or_else(|| w.strip_suffix('s').map(str::to_string)).unwrap_or_else(|| w.clone());
        ANIMAL_WORDS.contains(&w.as_str()) || ANIMAL_WORDS.contains(&singular.as_str())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
