/// Compute Levenshtein distance between two strings.
///
/// Uses Wagner-Fischer DP with early termination for short strings.
/// For typo tolerance, we typically only care about distances ≤ 1.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a_len = a.chars().count();
    let b_len = b.chars().count();

    if a_len == 0 {
        return b_len;
    }
    if b_len == 0 {
        return a_len;
    }

    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();

    let mut prev_row: Vec<usize> = (0..=b_len).collect();
    let mut curr_row = vec![0usize; b_len + 1];

    for i in 1..=a_len {
        curr_row[0] = i;
        for j in 1..=b_len {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            curr_row[j] = (curr_row[j - 1] + 1)
                .min(prev_row[j] + 1)
                .min(prev_row[j - 1] + cost);
        }
        std::mem::swap(&mut prev_row, &mut curr_row);
    }

    prev_row[b_len]
}

/// Check if two strings are within edit distance ≤ 1.
pub fn is_one_edit_away(a: &str, b: &str) -> bool {
    levenshtein(a, b) <= 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_levenshtein_identical() {
        assert_eq!(levenshtein("hello", "hello"), 0);
    }

    #[test]
    fn test_levenshtein_one_substitution() {
        assert_eq!(levenshtein("hello", "hallo"), 1);
    }

    #[test]
    fn test_levenshtein_one_insertion() {
        assert_eq!(levenshtein("hello", "helllo"), 1);
    }

    #[test]
    fn test_levenshtein_one_deletion() {
        assert_eq!(levenshtein("hello", "hell"), 1);
    }

    #[test]
    fn test_levenshtein_two_edits() {
        assert_eq!(levenshtein("hello", "helo"), 1);
        assert_eq!(levenshtein("hello", "heloh"), 2);
    }

    #[test]
    fn test_one_edit_away() {
        assert!(is_one_edit_away("hello", "hallo"));
        assert!(!is_one_edit_away("hello", "world"));
    }
}
