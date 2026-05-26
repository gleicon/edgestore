use std::collections::HashSet;

lazy_static::lazy_static! {
    static ref STOPWORDS: HashSet<String> = {
        let mut set = HashSet::new();
        let words = [
            "the", "a", "an", "is", "are", "was", "were", "be", "been", "being",
            "have", "has", "had", "do", "does", "did", "will", "would", "could", "should",
            "may", "might", "must", "shall", "can", "need", "dare", "ought", "used", "to",
            "of", "in", "for", "on", "with", "at", "by", "from", "as", "into",
            "through", "during", "before", "after", "above", "below", "between", "under",
            "and", "but", "or", "yet", "so", "if", "because", "although", "though", "while",
            "where", "when", "that", "which", "who", "whom", "whose", "what", "this", "these",
            "those", "such", "no", "nor", "not", "only", "own", "same", "each", "few",
            "more", "most", "other", "some", "very", "just", "now", "then", "here", "there",
            "up", "down", "out", "off", "over", "again", "further", "once",
        ];
        for w in words {
            set.insert(w.to_string());
        }
        set
    };
}

/// A token with its original position in the text.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    /// Stemmed, lowercased term.
    pub term: String,
    /// Zero-based position in the original text.
    pub position: usize,
}

/// Tokenize text into stemmed, lowercase, non-stopword tokens.
pub fn tokenize(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut position = 0usize;

    for word in text.split(|c: char| !c.is_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        let lower = word.to_lowercase();
        if STOPWORDS.contains(&lower) {
            continue;
        }
        let stemmed = stem(&lower);
        tokens.push(Token { term: stemmed, position });
        position += 1;
    }

    tokens
}

/// Simple English stemmer. Strips common suffixes.
fn stem(word: &str) -> String {
    if word.len() <= 2 {
        return word.to_string();
    }

    // Handle 'ies' → 'y' (babies → baby)
    if word.ends_with("ies") && word.len() > 4 {
        let base = &word[..word.len() - 3];
        if !base.ends_with('e') {
            return format!("{}y", base);
        }
    }

    // Handle 'es' → 'e' for specific endings
    if word.ends_with("es") && word.len() > 3 {
        let base = &word[..word.len() - 2];
        if base.ends_with("ch")
            || base.ends_with("sh")
            || base.ends_with("ss")
            || base.ends_with("x")
            || base.ends_with("z")
            || base.ends_with("o")
        {
            return base.to_string();
        }
    }

    // Handle 's' plural (but not for words ending in s-sibilants)
    if word.ends_with('s') && word.len() > 3 {
        let base = &word[..word.len() - 1];
        // Don't strip if the base ends with s, x, z, ch, sh
        if !base.ends_with('s')
            && !base.ends_with('x')
            && !base.ends_with('z')
            && !base.ends_with("ch")
            && !base.ends_with("sh")
        {
            return base.to_string();
        }
    }

    // Handle 'ing'
    if word.ends_with("ing") && word.len() > 5 {
        let base = &word[..word.len() - 3];
        // If base ends with a repeated consonant, keep one
        if base.len() > 1 && base.ends_with(base.chars().nth(base.len() - 2).unwrap()) {
            return base[..base.len() - 1].to_string();
        }
        return base.to_string();
    }

    // Handle 'ed'
    if word.ends_with("ed") && word.len() > 4 {
        let base = &word[..word.len() - 2];
        // If base ends with a repeated consonant, keep one
        if base.len() > 1 && base.ends_with(base.chars().nth(base.len() - 2).unwrap()) {
            return base[..base.len() - 1].to_string();
        }
        return base.to_string();
    }

    // Handle 'ly'
    if word.ends_with("ly") && word.len() > 4 {
        return word[..word.len() - 2].to_string();
    }

    word.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_basic() {
        let tokens = tokenize("Hello world");
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].term, "hello");
        assert_eq!(tokens[1].term, "world");
    }

    #[test]
    fn test_tokenize_punctuation() {
        let tokens = tokenize("Hello, world!");
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].term, "hello");
        assert_eq!(tokens[1].term, "world");
    }

    #[test]
    fn test_tokenize_stopwords() {
        let tokens = tokenize("The quick brown fox");
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[0].term, "quick");
        assert_eq!(tokens[1].term, "brown");
        assert_eq!(tokens[2].term, "fox");
    }

    #[test]
    fn test_stem_ing() {
        assert_eq!(stem("running"), "run");
        assert_eq!(stem("jumping"), "jump");
    }

    #[test]
    fn test_stem_ed() {
        assert_eq!(stem("jumped"), "jump");
        assert_eq!(stem("walked"), "walk");
    }

    #[test]
    fn test_stem_ies() {
        assert_eq!(stem("babies"), "baby");
        assert_eq!(stem("ponies"), "pony");
    }

    #[test]
    fn test_stem_s() {
        assert_eq!(stem("cats"), "cat");
        assert_eq!(stem("dogs"), "dog");
    }

    #[test]
    fn test_tokenize_empty() {
        let tokens = tokenize("");
        assert!(tokens.is_empty());
    }

    #[test]
    fn test_tokenize_positions() {
        let tokens = tokenize("alpha beta gamma");
        assert_eq!(tokens[0].position, 0);
        assert_eq!(tokens[1].position, 1);
        assert_eq!(tokens[2].position, 2);
    }
}
