use crate::text::types::FacetValue;
use crate::text::index::Posting;

/// A filter to apply to search results based on facet values.
#[derive(Debug, Clone)]
pub struct FacetFilter {
    pub field: String,
    pub value: FacetValue,
}

/// Filter postings to only those matching all facet filters.
pub fn filter_by_facets(postings: &[Posting], filters: &[FacetFilter]) -> Vec<Posting> {
    if filters.is_empty() {
        return postings.to_vec();
    }

    postings
        .iter()
        .filter(|posting| {
            filters.iter().all(|filter| {
                match posting.facets.get(&filter.field) {
                    Some(facet_value) => facet_value == &filter.value,
                    None => false,
                }
            })
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_posting(doc_id: u8, facets: HashMap<String, FacetValue>) -> Posting {
        Posting {
            doc_id: vec![doc_id],
            term_freq: 1,
            doc_len: 10,
            facets,
        }
    }

    #[test]
    fn test_filter_by_facets_exact_match() {
        let mut facets1 = HashMap::new();
        facets1.insert("category".to_string(), FacetValue::String("news".to_string()));
        let p1 = make_posting(1, facets1);

        let mut facets2 = HashMap::new();
        facets2.insert("category".to_string(), FacetValue::String("sports".to_string()));
        let p2 = make_posting(2, facets2);

        let filters = vec![FacetFilter {
            field: "category".to_string(),
            value: FacetValue::String("news".to_string()),
        }];

        let result = filter_by_facets(&[p1.clone(), p2.clone()], &filters);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].doc_id, vec![1]);
    }

    #[test]
    fn test_filter_by_facets_empty_filters() {
        let mut facets = HashMap::new();
        facets.insert("category".to_string(), FacetValue::String("news".to_string()));
        let p = make_posting(1, facets);

        let result = filter_by_facets(&[p.clone()], &[]);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_filter_by_facets_no_match() {
        let mut facets = HashMap::new();
        facets.insert("category".to_string(), FacetValue::String("news".to_string()));
        let p = make_posting(1, facets);

        let filters = vec![FacetFilter {
            field: "category".to_string(),
            value: FacetValue::String("sports".to_string()),
        }];

        let result = filter_by_facets(&[p], &filters);
        assert!(result.is_empty());
    }

    #[test]
    fn test_filter_multiple_facets() {
        let mut facets1 = HashMap::new();
        facets1.insert("category".to_string(), FacetValue::String("news".to_string()));
        facets1.insert("published".to_string(), FacetValue::Bool(true));
        let p1 = make_posting(1, facets1);

        let mut facets2 = HashMap::new();
        facets2.insert("category".to_string(), FacetValue::String("news".to_string()));
        facets2.insert("published".to_string(), FacetValue::Bool(false));
        let p2 = make_posting(2, facets2);

        let filters = vec![
            FacetFilter {
                field: "category".to_string(),
                value: FacetValue::String("news".to_string()),
            },
            FacetFilter {
                field: "published".to_string(),
                value: FacetValue::Bool(true),
            },
        ];

        let result = filter_by_facets(&[p1.clone(), p2.clone()], &filters);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].doc_id, vec![1]);
    }
}
