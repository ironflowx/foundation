//! Ironflow Resource Name construction, matching and pattern validation.
//!
//! IRN format: `irn:ironflow:{org}:{project}:{type}:{env}:{id}`

use std::collections::HashSet;
use std::fmt;

/// Fixed arity of an IRN.
const SEGMENT_COUNT: usize = 7;

/// Splits a comma-separated list and trims each element.
///
/// Matching, validation and normalization all go through this, so a pattern
/// that passes validation is always parsed the same way at match time.
fn patterns(list: &str) -> impl Iterator<Item = &str> {
    list.split(',').map(str::trim)
}

/// Constructs an IRN from its components.
///
/// Returns `None` if any component contains a colon, which would produce a
/// malformed IRN with the wrong number of segments.
pub fn build(org: &str, project: &str, resource_type: &str, env: &str, id: &str) -> Option<String> {
    let parts = [org, project, resource_type, env, id];
    if parts.iter().any(|p| p.contains(':')) {
        return None;
    }
    Some(format!(
        "irn:ironflow:{org}:{project}:{resource_type}:{env}:{id}"
    ))
}

/// Checks whether a resource IRN matches a pattern IRN.
///
/// A segment of `*` matches any single segment, and a bare `*` matches any IRN.
pub fn matches(pattern: &str, resource: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let mut p = pattern.split(':');
    let mut r = resource.split(':');
    loop {
        match (p.next(), r.next()) {
            (None, None) => return true,
            (Some(p), Some(r)) if p == "*" || p == r => {}
            _ => return false,
        }
    }
}

/// Checks whether an action matches any pattern in a comma-separated list.
///
/// A pattern may be `*`, an exact action, or `prefix:*` for every action under
/// `prefix:`.
pub fn matches_action(patterns_list: &str, action: &str) -> bool {
    patterns(patterns_list).any(|p| {
        p == "*"
            || p == action
            || p.strip_suffix('*')
                .is_some_and(|prefix| prefix.ends_with(':') && action.starts_with(prefix))
    })
}

/// Checks whether a resource IRN matches any pattern in a comma-separated list.
pub fn matches_resource(patterns_list: &str, resource: &str) -> bool {
    patterns(patterns_list).any(|p| matches(p, resource))
}

/// Why a resource pattern was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternError {
    Empty,
    WrongArity {
        pattern: String,
        segments: usize,
    },
    WrongPrefix {
        pattern: String,
    },
    /// `position` is 1-based.
    EmptySegment {
        pattern: String,
        position: usize,
    },
    /// `position` is 1-based.
    Whitespace {
        pattern: String,
        position: usize,
    },
    /// `position` is 1-based.
    PartialWildcard {
        pattern: String,
        position: usize,
    },
}

fn segment(pattern: &str, position: usize) -> &str {
    pattern.split(':').nth(position - 1).unwrap_or_default()
}

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "resource pattern must not be empty"),
            Self::WrongArity { pattern, segments } => write!(
                f,
                "resource pattern {pattern:?} has {segments} segments, want {SEGMENT_COUNT} \
                 (irn:ironflow:{{org}}:{{project}}:{{type}}:{{env}}:{{id}}); \
                 use \"*\" to match every resource"
            ),
            Self::WrongPrefix { pattern } => {
                write!(
                    f,
                    "resource pattern {pattern:?} must start with \"irn:ironflow:\""
                )
            }
            Self::EmptySegment { pattern, position } => write!(
                f,
                "resource pattern {pattern:?} has an empty segment at position {position}"
            ),
            Self::Whitespace { pattern, position } => write!(
                f,
                "resource pattern {pattern:?} has whitespace in segment {position} ({:?}); \
                 segments are compared byte-for-byte",
                segment(pattern, *position)
            ),
            Self::PartialWildcard { pattern, position } => write!(
                f,
                "resource pattern {pattern:?} has a partial wildcard in segment {position} ({:?}); \
                 \"*\" matches a whole segment, it is not a prefix or suffix",
                segment(pattern, *position)
            ),
        }
    }
}

impl std::error::Error for PatternError {}

/// Reports whether a policy resource pattern can ever match a real IRN.
///
/// [`matches`] compares segment counts first, so a pattern with the wrong arity
/// would be stored and shown as an active rule while never matching anything.
///
/// A bare `*` is the match-everything escape hatch and the only pattern exempt
/// from the arity rule. Every other pattern needs exactly seven non-empty
/// segments with the literal `irn` and `ironflow` leading. The service segment
/// is deliberately not wildcardable.
pub fn validate_pattern(pattern: &str) -> Result<(), PatternError> {
    if pattern == "*" {
        return Ok(());
    }
    if pattern.is_empty() {
        return Err(PatternError::Empty);
    }

    let parts: Vec<&str> = pattern.split(':').collect();
    if parts.len() != SEGMENT_COUNT {
        return Err(PatternError::WrongArity {
            pattern: pattern.to_owned(),
            segments: parts.len(),
        });
    }
    if parts[0] != "irn" || parts[1] != "ironflow" {
        return Err(PatternError::WrongPrefix {
            pattern: pattern.to_owned(),
        });
    }
    for (i, p) in parts.iter().enumerate() {
        let (pattern, position) = (pattern.to_owned(), i + 1);
        if p.is_empty() {
            return Err(PatternError::EmptySegment { pattern, position });
        }
        // Segments are compared byte-for-byte, so whitespace makes a segment
        // unmatchable. The predicate is the one `trim` applies to the list.
        if p.chars().any(char::is_whitespace) {
            return Err(PatternError::Whitespace { pattern, position });
        }
        // `*` is only a whole-segment wildcard, so `my-*` is a literal that can
        // never match `my-fn`.
        if *p != "*" && p.contains('*') {
            return Err(PatternError::PartialWildcard { pattern, position });
        }
    }
    Ok(())
}

/// Validates a comma-separated pattern list, as consumed by [`matches_resource`].
pub fn validate_resources(patterns_list: &str) -> Result<(), PatternError> {
    if patterns_list.is_empty() {
        return Err(PatternError::Empty);
    }
    patterns(patterns_list).try_for_each(validate_pattern)
}

/// Returns the canonical form of a comma-separated pattern list: elements
/// trimmed, empties and duplicates dropped, order preserved, joined by a bare
/// comma.
///
/// Lets callers ask whether the pattern set changed rather than whether the
/// bytes changed, so a client that resubmits a reformatted list is not treated
/// as editing it.
pub fn normalize_resources(patterns_list: &str) -> String {
    let mut seen = HashSet::new();
    patterns(patterns_list)
        .filter(|p| !p.is_empty() && seen.insert(*p))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "irn:ironflow:acme:proj:function:prod:fn-1";

    #[test]
    fn build_refuses_colons() {
        assert_eq!(
            build("acme", "proj", "function", "prod", "fn-1").as_deref(),
            Some(GOOD)
        );
        for i in 0..5 {
            let mut parts = ["acme", "proj", "function", "prod", "fn-1"];
            parts[i] = "a:b";
            assert_eq!(
                build(parts[0], parts[1], parts[2], parts[3], parts[4]),
                None
            );
        }
    }

    #[test]
    fn wildcard_and_arity_matching() {
        assert!(matches("*", GOOD));
        assert!(matches("*", "anything"));
        assert!(matches(GOOD, GOOD));
        assert!(matches("irn:ironflow:acme:*:function:*:*", GOOD));
        assert!(!matches("irn:ironflow:other:*:function:*:*", GOOD));
        assert!(!matches("irn:ironflow:acme:*:function:*", GOOD));
        assert!(!matches("irn:ironflow:acme:*:function:*:*:*", GOOD));
        assert!(matches_resource(" nope , irn:ironflow:*:*:*:*:* ", GOOD));
        assert!(!matches_resource("nope,irn:ironflow:other:*:*:*:*", GOOD));
    }

    #[test]
    fn action_matching() {
        assert!(matches_action("*", "runs:cancel"));
        assert!(matches_action("runs:cancel", "runs:cancel"));
        assert!(matches_action("runs:*", "runs:cancel"));
        assert!(matches_action("a:b, runs:*", "runs:cancel"));
        assert!(!matches_action("run:*", "runs:cancel"));
        assert!(!matches_action("runs:list", "runs:cancel"));
    }

    #[test]
    fn pattern_errors() {
        assert_eq!(validate_pattern("*"), Ok(()));
        assert_eq!(validate_pattern("irn:ironflow:*:*:*:*:*"), Ok(()));
        assert_eq!(validate_pattern(""), Err(PatternError::Empty));
        assert_eq!(
            validate_pattern("irn:ironflow:a"),
            Err(PatternError::WrongArity {
                pattern: "irn:ironflow:a".into(),
                segments: 3
            })
        );
        assert_eq!(
            validate_pattern("urn:ironflow:a:b:c:d:e"),
            Err(PatternError::WrongPrefix {
                pattern: "urn:ironflow:a:b:c:d:e".into()
            })
        );
        assert_eq!(
            validate_pattern("irn:ironflow:a::c:d:e"),
            Err(PatternError::EmptySegment {
                pattern: "irn:ironflow:a::c:d:e".into(),
                position: 4
            })
        );
        assert_eq!(
            validate_pattern("irn:ironflow:a:b c:d:e:f"),
            Err(PatternError::Whitespace {
                pattern: "irn:ironflow:a:b c:d:e:f".into(),
                position: 4
            })
        );
        let nbsp = "irn:ironflow:a:b\u{a0}c:d:e:f";
        assert_eq!(
            validate_pattern(nbsp),
            Err(PatternError::Whitespace {
                pattern: nbsp.into(),
                position: 4
            })
        );
        assert_eq!(
            validate_pattern("irn:ironflow:a:b:c:d:fn-*"),
            Err(PatternError::PartialWildcard {
                pattern: "irn:ironflow:a:b:c:d:fn-*".into(),
                position: 7
            })
        );
    }

    #[test]
    fn error_display() {
        let err = validate_pattern("irn:ironflow:a:b:c:d:fn-*").unwrap_err();
        assert_eq!(
            err.to_string(),
            "resource pattern \"irn:ironflow:a:b:c:d:fn-*\" has a partial wildcard in segment 7 \
             (\"fn-*\"); \"*\" matches a whole segment, it is not a prefix or suffix"
        );
        let err: &dyn std::error::Error = &PatternError::Empty;
        assert_eq!(err.to_string(), "resource pattern must not be empty");
    }

    #[test]
    fn validate_resources_trims_like_matching() {
        assert_eq!(validate_resources("*"), Ok(()));
        assert_eq!(validate_resources(" * , irn:ironflow:*:*:*:*:* "), Ok(()));
        assert_eq!(validate_resources(""), Err(PatternError::Empty));
        assert_eq!(
            validate_resources("a,,b"),
            Err(PatternError::WrongArity {
                pattern: "a".into(),
                segments: 1
            })
        );
        assert_eq!(validate_resources("*,,*"), Err(PatternError::Empty));
    }

    #[test]
    fn normalize_resources_dedupes_in_order() {
        assert_eq!(normalize_resources(" b, a,,b , a"), "b,a");
        assert_eq!(normalize_resources(" , "), "");
    }
}
