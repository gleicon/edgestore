use std::collections::HashSet;
use std::sync::LazyLock;

use rust_stemmers::{Algorithm, Stemmer};

/// Language for text analysis (tokenization, stemming, stopword filtering).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Language {
    #[default]
    English,
    PortugueseBrazilian,
}

impl Language {
    fn algorithm(self) -> Algorithm {
        match self {
            Language::English => Algorithm::English,
            Language::PortugueseBrazilian => Algorithm::Portuguese,
        }
    }

    fn stop_words_code(self) -> stop_words::LANGUAGE {
        match self {
            Language::English => stop_words::LANGUAGE::English,
            Language::PortugueseBrazilian => stop_words::LANGUAGE::Portuguese,
        }
    }
}

static EN_STOPWORDS: LazyLock<HashSet<String>> =
    LazyLock::new(|| stop_words::get(stop_words::LANGUAGE::English).into_iter().collect());

static PT_STOPWORDS: LazyLock<HashSet<String>> =
    LazyLock::new(|| stop_words::get(stop_words::LANGUAGE::Portuguese).into_iter().collect());

fn stopwords_for(lang: Language) -> &'static HashSet<String> {
    match lang {
        Language::English => &EN_STOPWORDS,
        Language::PortugueseBrazilian => &PT_STOPWORDS,
    }
}

/// A token with its original position in the text.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    /// Stemmed, lowercased term.
    pub term: String,
    /// Zero-based character index of the token's start in the original (pre-stem) text.
    pub position: usize,
}

/// Tokenize `text` into stemmed, lowercase, non-stopword tokens for the given language.
///
/// Uses Snowball stemming (`rust-stemmers`) and curated stopword lists (`stop-words`).
pub fn tokenize(text: &str, lang: Language) -> Vec<Token> {
    let stemmer = Stemmer::create(lang.algorithm());
    let stopwords = stopwords_for(lang);
    let mut tokens = Vec::new();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let char_count = chars.len();
    let mut i = 0;

    while i < char_count {
        if chars[i].1.is_alphanumeric() {
            let char_start = i;
            let byte_start = chars[i].0;
            let mut j = i + 1;
            while j < char_count && chars[j].1.is_alphanumeric() {
                j += 1;
            }
            let byte_end = if j < char_count { chars[j].0 } else { text.len() };
            let word = &text[byte_start..byte_end];
            let lower = word.to_lowercase();
            if !stopwords.contains(lower.as_str()) {
                let stemmed = stemmer.stem(&lower).into_owned();
                tokens.push(Token {
                    term: stemmed,
                    position: char_start,
                });
            }
            i = j;
        } else {
            i += 1;
        }
    }

    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_basic() {
        let tokens = tokenize("database storage engine", Language::English);
        assert!(!tokens.is_empty());
        // all terms should be non-empty strings
        assert!(tokens.iter().all(|t| !t.term.is_empty()));
    }

    #[test]
    fn test_tokenize_punctuation() {
        // punctuation splits tokens; same content words should appear with or without it
        let without = tokenize("database storage", Language::English);
        let with_punct = tokenize("database, storage!", Language::English);
        assert_eq!(without.len(), with_punct.len());
    }

    #[test]
    fn test_tokenize_stopwords_en() {
        let tokens = tokenize("the quick brown fox", Language::English);
        // "the" is a stopword in every English list
        assert!(tokens.iter().all(|t| t.term != "the"));
    }

    #[test]
    fn test_snowball_stemming_en() {
        let tokens = tokenize("running jumped studies happiness", Language::English);
        // Snowball should stem these correctly
        let terms: Vec<&str> = tokens.iter().map(|t| t.term.as_str()).collect();
        assert!(terms.contains(&"run") || terms.contains(&"runn"), "expected 'running' stemmed");
        assert!(terms.contains(&"jump"), "expected 'jumped' stemmed to 'jump'");
        assert!(terms.contains(&"studi") || terms.contains(&"study"), "expected 'studies' stemmed");
        assert!(terms.contains(&"happi") || terms.contains(&"happiness"), "expected 'happiness' stemmed");
    }

    #[test]
    fn test_tokenize_portuguese() {
        let tokens = tokenize("Os gatos correm rápido", Language::PortugueseBrazilian);
        // "os" is a Portuguese stopword
        assert!(tokens.iter().all(|t| t.term != "os"));
        assert!(!tokens.is_empty());
    }

    #[test]
    fn test_tokenize_empty() {
        let tokens = tokenize("", Language::English);
        assert!(tokens.is_empty());
    }

    #[test]
    fn test_tokenize_positions() {
        let tokens = tokenize("alpha beta gamma", Language::English);
        assert_eq!(tokens[0].position, 0);
        assert_eq!(tokens[1].position, 6);
        assert_eq!(tokens[2].position, 11);
    }
}
