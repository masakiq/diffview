/// Adjusts `scroll` so `display_row` stays within `[scroll, scroll + pane_height)`.
/// `display_row` must already be in the caller's own row space (`patch_cursor` is a
/// display row itself; `diff_cursor`/full-file cursor need converting first) — this
/// function only does the shared viewport-follow math, not the coordinate conversion.
pub fn follow(display_row: usize, scroll: &mut usize, pane_height: usize) {
    if display_row < *scroll {
        *scroll = display_row;
    } else if display_row >= *scroll + pane_height {
        *scroll = display_row + 1 - pane_height;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrolls_up_when_row_is_above_the_viewport() {
        let mut scroll = 10;
        follow(3, &mut scroll, 5);
        assert_eq!(scroll, 3);
    }

    #[test]
    fn scrolls_down_when_row_is_below_the_viewport() {
        let mut scroll = 0;
        follow(20, &mut scroll, 5);
        assert_eq!(scroll, 16);
    }

    #[test]
    fn leaves_scroll_untouched_when_row_is_already_visible() {
        let mut scroll = 5;
        follow(7, &mut scroll, 5);
        assert_eq!(scroll, 5);
    }

    #[test]
    fn leaves_scroll_untouched_at_the_exact_top_edge() {
        let mut scroll = 5;
        follow(5, &mut scroll, 5);
        assert_eq!(scroll, 5);
    }

    #[test]
    fn scrolls_when_row_is_exactly_at_the_bottom_edge() {
        let mut scroll = 0;
        follow(5, &mut scroll, 5);
        assert_eq!(scroll, 1);
    }
}
