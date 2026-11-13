//! The lowest version that a declared requirement allows, which must not be
//! older than the one the generated code needs.

use std::cmp::Ordering;

/// The lowest version of a package that a requirement allows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Floor {
    /// The versions from this release on, such as `[2, 7]` for 2.7.
    Release(Vec<u64>),
    /// Every version.
    Any,
    /// A source that names no versions, such as a URL, a path or a tag,
    /// which nothing here can compare.
    Unread,
}

impl Floor {
    /// Read the floor of a Python requirement as PEP 508 writes it, such as
    /// `pydantic>=2.7,<3`. Clauses joined by commas must all hold, so the
    /// floor is the highest that one sets.
    pub(crate) fn python(requirement: &str) -> Self {
        let requirement = requirement.split(';').next().unwrap_or_default();
        let rest = requirement
            .trim_start()
            .trim_start_matches(|character: char| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
            });
        let rest = rest.trim_start();
        let rest = match rest.strip_prefix('[') {
            Some(extras) => extras.split_once(']').map_or("", |(_, rest)| rest),
            None => rest,
        };
        let rest = rest.trim();
        if rest.starts_with('@') {
            return Self::Unread;
        }
        let specifiers = rest.trim_start_matches('(').trim_end_matches(')');
        let mut floor = Self::Any;
        for clause in specifiers
            .split(',')
            .map(str::trim)
            .filter(|clause| !clause.is_empty())
        {
            let operator_length = clause
                .find(|character: char| !matches!(character, '<' | '>' | '=' | '!' | '~'))
                .unwrap_or(clause.len());
            let (operator, version) = clause.split_at(operator_length);
            let bound = match operator {
                ">=" | ">" | "==" | "===" | "~=" => release(version.trim()),
                "<" | "<=" | "!=" => continue,
                _ => None,
            };
            let Some(bound) = bound else {
                return Self::Unread;
            };
            floor = floor.max(Self::Release(bound));
        }
        floor
    }

    /// Read the floor of an npm range, such as `^21.2.0` or `>=21 <23`.
    /// Comparators joined by spaces must all hold, so an alternative's floor
    /// is the highest that one sets, and the range's is the lowest of its
    /// alternatives.
    pub(crate) fn npm(range: &str) -> Self {
        let range = range.trim();
        // Workspace, file, alias, Git and other sources name no versions.
        if range.contains(':') || range.contains('/') {
            return Self::Unread;
        }
        let mut lowest: Option<Self> = None;
        for alternative in range.split("||").map(str::trim) {
            let floor = match alternative.split_once(" - ") {
                Some((from, _)) => release(from.trim()).map_or(Self::Unread, Self::Release),
                None => comparators_floor(alternative),
            };
            if floor == Self::Unread {
                return floor;
            }
            lowest = Some(match lowest {
                Some(lowest) => lowest.min(floor),
                None => floor,
            });
        }
        lowest.unwrap_or(Self::Any)
    }

    /// Whether this floor allows a release older than `needed`. A floor that
    /// nothing can read allows none that anything here can tell.
    pub(crate) fn below(&self, needed: &Self) -> bool {
        match (self, needed) {
            (Self::Unread, _) | (_, Self::Any | Self::Unread) => false,
            (Self::Any, Self::Release(_)) => true,
            (Self::Release(floor), Self::Release(needed)) => {
                compare(floor, needed) == Ordering::Less
            }
        }
    }

    /// The higher of two floors that both hold; `Any` is the lowest.
    fn max(self, other: Self) -> Self {
        match (self, other) {
            (Self::Release(left), Self::Release(right)) => {
                if compare(&left, &right) == Ordering::Less {
                    Self::Release(right)
                } else {
                    Self::Release(left)
                }
            }
            (Self::Any, floor) | (floor, Self::Any) => floor,
            (Self::Unread, _) | (_, Self::Unread) => Self::Unread,
        }
    }

    /// The lower of two floors of which either may hold; `Any` is the lowest.
    fn min(self, other: Self) -> Self {
        match (self, other) {
            (Self::Release(left), Self::Release(right)) => {
                if compare(&left, &right) == Ordering::Less {
                    Self::Release(left)
                } else {
                    Self::Release(right)
                }
            }
            (Self::Any, _) | (_, Self::Any) => Self::Any,
            (Self::Unread, _) | (_, Self::Unread) => Self::Unread,
        }
    }
}

/// The floor of one npm alternative of comparators joined by spaces, such as
/// `>=21 <23`. `^`, `~`, `>=`, `>` and `=` set one, and `<` and `<=` none.
fn comparators_floor(alternative: &str) -> Floor {
    let mut floor = Floor::Any;
    for comparator in alternative.split_whitespace() {
        if comparator.starts_with('<') {
            continue;
        }
        let version = comparator
            .trim_start_matches(['>', '=', '^', '~'])
            .trim_start_matches('v');
        if matches!(version, "" | "*" | "x" | "X") {
            continue;
        }
        let Some(bound) = release(version) else {
            return Floor::Unread;
        };
        floor = floor.max(Floor::Release(bound));
    }
    floor
}

/// Read the release numbers at the start of a version, such as `[2, 7]` from
/// `2.7`, `2.7.*`, `2.7.x` or `2.7rc1`: a pre-release counts as its release.
fn release(version: &str) -> Option<Vec<u64>> {
    let version = version.strip_prefix('v').unwrap_or(version);
    let version = version
        .split_once('!')
        .map_or(version, |(_, release)| release);
    let mut numbers = Vec::new();
    for part in version.split('.') {
        let digits = part
            .find(|character: char| !character.is_ascii_digit())
            .map_or(part, |end| &part[..end]);
        let Ok(number) = digits.parse() else {
            break;
        };
        numbers.push(number);
        if digits.len() < part.len() {
            break;
        }
    }
    (!numbers.is_empty()).then_some(numbers)
}

/// Compare two releases, whose missing numbers are zeros.
fn compare(left: &[u64], right: &[u64]) -> Ordering {
    let length = left.len().max(right.len());
    let number = |release: &[u64], index: usize| release.get(index).copied().unwrap_or(0);
    (0..length)
        .map(|index| number(left, index).cmp(&number(right, index)))
        .find(|ordering| ordering.is_ne())
        .unwrap_or(Ordering::Equal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_requirements_set_the_highest_lower_bound_of_their_clauses() {
        let release = |numbers: &[u64]| Floor::Release(numbers.to_vec());

        assert_eq!(Floor::python("pydantic>=2.7,<3"), release(&[2, 7]));
        assert_eq!(Floor::python("pydantic >= 2, >2.8"), release(&[2, 8]));
        assert_eq!(Floor::python("pydantic~=2.9"), release(&[2, 9]));
        assert_eq!(Floor::python("pydantic==2.*"), release(&[2]));
        assert_eq!(
            Floor::python("pydantic[email] (>=2.10); python_version>'3'"),
            release(&[2, 10])
        );
        assert_eq!(Floor::python("pydantic"), Floor::Any);
        assert_eq!(Floor::python("pydantic<3"), Floor::Any);
        assert_eq!(
            Floor::python("pydantic @ https://example.com/p.whl"),
            Floor::Unread
        );
    }

    #[test]
    fn npm_ranges_set_the_lowest_floor_of_their_alternatives() {
        let release = |numbers: &[u64]| Floor::Release(numbers.to_vec());

        assert_eq!(Floor::npm("^21.2.0"), release(&[21, 2, 0]));
        assert_eq!(Floor::npm(">=21 <23"), release(&[21]));
        assert_eq!(Floor::npm("~21.3"), release(&[21, 3]));
        assert_eq!(Floor::npm("21.x"), release(&[21]));
        assert_eq!(Floor::npm("21.2.0 - 22"), release(&[21, 2, 0]));
        assert_eq!(Floor::npm("^22 || ^21.4"), release(&[21, 4]));
        assert_eq!(Floor::npm("*"), Floor::Any);
        assert_eq!(Floor::npm("^22 || *"), Floor::Any);
        assert_eq!(Floor::npm("workspace:*"), Floor::Unread);
        assert_eq!(Floor::npm("latest"), Floor::Unread);
    }

    #[test]
    fn a_floor_below_the_needed_release_allows_older_versions() {
        let needed = Floor::python("pydantic>=2.7,<3");

        assert!(Floor::python("pydantic>=2,<3").below(&needed));
        assert!(Floor::python("pydantic").below(&needed));
        assert!(!Floor::python("pydantic>=2.7.0").below(&needed));
        assert!(!Floor::python("pydantic>=2.10").below(&needed));
        assert!(!Floor::python("pydantic @ file:///wheels/p.whl").below(&needed));
        assert!(Floor::npm(">=21").below(&Floor::npm("^21.2.0")));
        assert!(!Floor::npm("^21.2.0").below(&Floor::npm("^21.2.0")));
    }
}
