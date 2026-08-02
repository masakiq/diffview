pub fn next_match_from(matches: &[usize], current: usize, inclusive: bool) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }

    let predicate = |candidate: &usize| {
        if inclusive {
            *candidate >= current
        } else {
            *candidate > current
        }
    };

    matches
        .iter()
        .copied()
        .find(predicate)
        .or_else(|| matches.first().copied())
}

pub fn prev_match_from(matches: &[usize], current: usize, inclusive: bool) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }

    let predicate = |candidate: &usize| {
        if inclusive {
            *candidate <= current
        } else {
            *candidate < current
        }
    };

    matches
        .iter()
        .copied()
        .rev()
        .find(predicate)
        .or_else(|| matches.last().copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_match_wraps_forward() {
        let matches = vec![2, 5, 9];

        assert_eq!(next_match_from(&matches, 2, false), Some(5));
        assert_eq!(next_match_from(&matches, 9, false), Some(2));
        assert_eq!(next_match_from(&matches, 5, true), Some(5));
    }

    #[test]
    fn prev_match_wraps_backward() {
        let matches = vec![2, 5, 9];

        assert_eq!(prev_match_from(&matches, 5, false), Some(2));
        assert_eq!(prev_match_from(&matches, 2, false), Some(9));
        assert_eq!(prev_match_from(&matches, 5, true), Some(5));
    }
}
