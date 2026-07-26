use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
};

fn search_highlight_style() -> Style {
    Style::default()
        .bg(Color::Yellow)
        .add_modifier(Modifier::BOLD)
}

/// Whether `query_lower` matches `text`'s characters (from `chars`, `text`'s own
/// `char_indices()`) starting at `chars[start_idx]`, comparing case-insensitively one
/// original character at a time. Returns the char index (an index into `chars`, one past
/// the last original character consumed) on a match — always one of `text`'s own char
/// boundaries once resolved back to a byte offset, never a position computed from some
/// other, separately case-converted string.
///
/// A single original character can lowercase into *multiple* characters (`'İ'` → `'i'` +
/// a combining dot, 2 bytes → 3 bytes) — comparing `ch.to_lowercase()`'s whole output
/// against consecutive query characters before advancing `ti` means such a character is
/// only ever consumed as one indivisible unit, so a match's end can't land mid-character.
///
/// If the query runs out partway through one character's lowercase expansion (e.g. query
/// `"i"` against `'İ'`, whose expansion is `"i"` + a combining dot), that still counts as
/// a match of the whole original character — the alternative (rejecting it) would make an
/// ordinary-looking search silently stop matching text a user can plainly see it in.
fn match_at(chars: &[(usize, char)], start_idx: usize, query_lower: &[char]) -> Option<usize> {
    let mut ti = start_idx;
    let mut qi = 0;
    while qi < query_lower.len() {
        let (_, ch) = *chars.get(ti)?;
        for lowered in ch.to_lowercase() {
            if qi >= query_lower.len() {
                break;
            }
            if query_lower[qi] != lowered {
                return None;
            }
            qi += 1;
        }
        ti += 1;
    }
    Some(ti)
}

/// Case-insensitive match byte ranges within `text`. Byte offsets are always `text`'s own
/// char boundaries: matching happens character by character directly against `text` (via
/// `match_at`), never by lowercasing the whole string into a separate buffer and reusing
/// its byte offsets — `str::to_lowercase()` isn't byte-length-preserving for every
/// character, so offsets found in a separately lowered copy aren't valid offsets into the
/// original and slicing at them can panic on such input (e.g. `'İ'`, 2 bytes, lowercases
/// to 3 bytes).
///
/// This is the single matcher shared by both App's search-hit detection (`contains_match`)
/// and the UI's highlight rendering below it — using two different matchers (whole-string
/// `to_lowercase` vs per-character) let them disagree on Unicode input (e.g. App finding a
/// line `n`/`N` can navigate to, that the UI then declines to highlight, or vice versa).
fn match_ranges(text: &str, query: &str) -> Vec<(usize, usize)> {
    if query.is_empty() {
        return Vec::new();
    }

    let query_lower: Vec<char> = query.to_lowercase().chars().collect();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let byte_at = |char_idx: usize| chars.get(char_idx).map_or(text.len(), |(byte, _)| *byte);

    let mut ranges = Vec::new();
    let mut idx = 0;
    while idx < chars.len() {
        if let Some(end_idx) = match_at(&chars, idx, &query_lower) {
            ranges.push((chars[idx].0, byte_at(end_idx)));
            idx = end_idx;
        } else {
            idx += 1;
        }
    }

    ranges
}

/// Whether `query` matches anywhere in `text`, using the same per-character Unicode-aware
/// rules as the highlight ranges below — so a search hit (`n`/`N` navigation, `No matches`)
/// and its on-screen highlight never disagree on whether a given line matches.
pub fn contains_match(text: &str, query: &str) -> bool {
    !match_ranges(text, query).is_empty()
}

/// Applies already-computed byte ranges (each a match to highlight) to `spans`, splitting
/// spans at range boundaries as needed. `ranges` must be sorted, non-overlapping byte
/// offsets into the spans' joined content — callers that already know the ranges (e.g.
/// full-file view, which computes them against raw content and shifts them into display
/// coordinates) call this directly instead of `match_ranges` re-deriving them from the
/// (possibly decorated) span text.
fn apply_highlight_ranges<'a>(spans: Vec<Span<'a>>, ranges: &[(usize, usize)]) -> Vec<Span<'a>> {
    if ranges.is_empty() {
        return spans;
    }

    let mut highlighted = Vec::new();
    let mut global_offset = 0;

    for span in spans {
        let text = span.content.into_owned();
        let span_start = global_offset;
        let span_end = span_start + text.len();
        let mut local_offset = 0;

        for &(match_start, match_end) in ranges {
            if match_end <= span_start || match_start >= span_end {
                continue;
            }

            let local_start = match_start.saturating_sub(span_start);
            let local_end = (match_end.min(span_end)).saturating_sub(span_start);

            if local_offset < local_start {
                highlighted.push(Span::styled(
                    text[local_offset..local_start].to_string(),
                    span.style,
                ));
            }

            highlighted.push(Span::styled(
                text[local_start..local_end].to_string(),
                span.style.patch(search_highlight_style()),
            ));

            local_offset = local_end;
        }

        if local_offset < text.len() {
            highlighted.push(Span::styled(text[local_offset..].to_string(), span.style));
        }

        global_offset = span_end;
    }

    highlighted
}

pub fn highlight_spans<'a>(spans: Vec<Span<'a>>, query: Option<&str>) -> Vec<Span<'a>> {
    let Some(query) = query.filter(|query| !query.is_empty()) else {
        return spans;
    };

    let joined = spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    let ranges = match_ranges(&joined, query);
    apply_highlight_ranges(spans, &ranges)
}

pub fn highlight_line<'a>(spans: Vec<Span<'a>>, query: Option<&str>) -> Line<'a> {
    Line::from(highlight_spans(spans, query))
}

pub fn highlight_text<'a>(text: Text<'a>, query: Option<&str>) -> Text<'a> {
    let Some(query) = query.filter(|query| !query.is_empty()) else {
        return text;
    };

    let lines = text
        .lines
        .into_iter()
        .map(|line| Line {
            style: line.style,
            alignment: line.alignment,
            spans: highlight_spans(line.spans, Some(query)),
        })
        .collect();

    Text {
        alignment: text.alignment,
        style: text.style,
        lines,
    }
}

/// Byte offset of the first byte of actual file content in a row of bat's `numbers,grid`
/// output (see `FULL_FILE_VIEW_BAT_STYLE` in `git/diff.rs`), or the row's full length when
/// there's no `│` at all (the top/bottom border rows, which use `┬`/`┴` instead). Skips both
/// the gutter separator itself and the one padding space `numbers,grid` always inserts right
/// after it on every row, including empty ones — without skipping that space too, a query
/// starting with a space (or a bare space) would match bat's own decoration instead of real
/// content that starts one byte later.
fn full_file_content_start(row_text: &str) -> usize {
    let Some(sep_idx) = row_text.find('│') else {
        return row_text.len();
    };
    let after_sep = sep_idx + '│'.len_utf8();
    if row_text[after_sep..].starts_with(' ') {
        after_sep + 1
    } else {
        after_sep
    }
}

/// Like `highlight_text`, but for full-file view: rather than re-searching bat's rendered
/// display text (which has gutter/border decoration, cursor/tint padding out to the pane's
/// width, and — via `--tabs=1` — every raw tab byte turned into a display space), matches
/// are found once against `raw_content`'s own lines, the exact same text
/// `App::searchable_lines_for_scope` already searches for `n`/`N` navigation. Each match's
/// byte range is then shifted by `full_file_content_start` to land on the equivalent bytes
/// in the display row.
///
/// That shift is exact — not approximate — because `render_content_preview`'s forced `bat`
/// flags (`--wrap=never`, `--tabs=1`, `--no-config`) guarantee every raw content byte maps
/// to exactly one display byte at the same relative offset past the gutter (a literal tab
/// becomes a literal space; nothing else changes length). Matching only against real raw
/// content, never the display text, also means a match can never land inside the gutter,
/// a border row, or the trailing padding `tint_line_bg` adds out to the pane width — those
/// bytes simply don't exist in `raw_content`, so no exclusion logic is needed for them.
pub fn highlight_full_file_text<'a>(
    text: Text<'a>,
    query: Option<&str>,
    raw_content: &str,
    content_offset: usize,
) -> Text<'a> {
    let Some(query) = query.filter(|query| !query.is_empty()) else {
        return text;
    };

    let raw_lines: Vec<&str> = raw_content.lines().collect();

    let lines = text
        .lines
        .into_iter()
        .enumerate()
        .map(|(row, line)| {
            let raw_line = row
                .checked_sub(content_offset)
                .and_then(|raw_idx| raw_lines.get(raw_idx));
            let Some(raw_line) = raw_line else {
                return line;
            };

            let raw_ranges = match_ranges(raw_line, query);
            if raw_ranges.is_empty() {
                return line;
            }

            let joined: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            let content_start = full_file_content_start(&joined);
            let shifted: Vec<(usize, usize)> = raw_ranges
                .iter()
                .map(|&(start, end)| (content_start + start, content_start + end))
                .collect();

            Line {
                style: line.style,
                alignment: line.alignment,
                spans: apply_highlight_ranges(line.spans, &shifted),
            }
        })
        .collect();

    Text {
        alignment: text.alignment,
        style: text.style,
        lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_matches_across_span_boundaries() {
        let spans = vec![
            Span::raw("foo"),
            Span::styled("bar", Style::default().fg(Color::Green)),
        ];

        let highlighted = highlight_spans(spans, Some("oob"));

        assert_eq!(highlighted.len(), 4);
        assert_eq!(highlighted[0].content.as_ref(), "f");
        assert_eq!(highlighted[1].content.as_ref(), "oo");
        assert_eq!(highlighted[2].content.as_ref(), "b");
        assert_eq!(highlighted[3].content.as_ref(), "ar");
        assert_eq!(highlighted[2].style.fg, Some(Color::Green));
        assert_eq!(highlighted[1].style.bg, Some(Color::Yellow));
        assert_eq!(highlighted[2].style.bg, Some(Color::Yellow));
    }

    #[test]
    fn highlights_matches_case_insensitively() {
        let spans = vec![Span::raw("FooBar")];

        let highlighted = highlight_spans(spans, Some("oba"));

        assert_eq!(highlighted.len(), 3);
        assert_eq!(highlighted[1].content.as_ref(), "oBa");
        assert_eq!(highlighted[1].style.bg, Some(Color::Yellow));
    }

    #[test]
    fn highlighting_a_character_that_expands_under_lowercasing_does_not_panic() {
        // 'İ' (U+0130) lowercases to "i" + a combining dot above — 2 bytes become 3.
        // Computing match byte ranges against a separately-lowered copy of the text and
        // then slicing the ORIGINAL (non-lowered) text at those offsets panics on a byte
        // that only exists mid-character in the original ("byte index 1 is not a char
        // boundary"). Matching directly against the original text's own char boundaries
        // must not panic.
        let spans = vec![Span::raw("İstanbul")];

        let highlighted = highlight_spans(spans, Some("i"));

        let rebuilt: String = highlighted.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(rebuilt, "İstanbul");
    }

    #[test]
    fn a_query_ending_mid_character_expansion_still_highlights_the_whole_character() {
        // Query "i" only matches the "i" half of 'İ''s two-character lowercase expansion
        // ("i" + a combining dot) — that must still highlight the whole original 'İ', not
        // be rejected as a non-match, so App's search-hit detection (contains_match, which
        // shares this same matcher) and the on-screen highlight always agree.
        assert!(contains_match("İstanbul", "i"));

        let spans = vec![Span::raw("İstanbul")];
        let highlighted = highlight_spans(spans, Some("i"));
        assert_eq!(highlighted[0].content.as_ref(), "İ");
        assert_eq!(highlighted[0].style.bg, Some(Color::Yellow));
    }

    #[test]
    fn full_file_content_start_skips_the_numbered_gutter_and_its_padding_space() {
        let row = "   1 │ needle";
        let sep_end = row.find('│').unwrap() + '│'.len_utf8();
        // The byte right after `│` is bat's own padding space, not real content — the
        // boundary must land one byte further, at "needle" itself.
        assert_eq!(full_file_content_start(row), sep_end + 1);
        assert_eq!(&row[full_file_content_start(row)..], "needle");
    }

    #[test]
    fn full_file_content_start_returns_full_length_for_border_rows_without_a_separator() {
        let border = "─────┬─────────────";
        assert_eq!(full_file_content_start(border), border.len());
    }

    #[test]
    fn highlight_full_file_text_matches_only_raw_content_never_the_gutter_or_border_rows() {
        let raw_content = "needle\nhas a 1 in it";
        let text = Text::from(vec![
            Line::from(Span::raw("─────┬─────────────")), // border row (row 0)
            Line::from(Span::raw("   1 │ needle")),       // raw line 0 (row 1)
            Line::from(Span::raw("   2 │ has a 1 in it")), // raw line 1 (row 2)
        ]);

        let highlighted = highlight_full_file_text(text, Some("1"), raw_content, 1);

        // Border row: below content_offset, never even considered.
        assert!(highlighted.lines[0]
            .spans
            .iter()
            .all(|s| s.style.bg.is_none()));

        // Row 1's raw line "needle" has no "1" at all — the gutter's own line-number "1"
        // must never be matched, because matching happens against the raw line only.
        assert!(highlighted.lines[1]
            .spans
            .iter()
            .all(|s| s.style.bg.is_none()));

        // Row 2's "1" is inside the actual raw content — it is highlighted.
        assert!(highlighted.lines[2]
            .spans
            .iter()
            .any(|s| s.style.bg == Some(Color::Yellow) && s.content.as_ref() == "1"));
    }

    #[test]
    fn highlight_full_file_text_does_not_highlight_bats_tab_expansion_as_a_literal_space() {
        // The raw line has a literal tab, not a space. bat's forced `--tabs=1` renders it
        // as a single display space, but a query of a bare space must not match — the raw
        // content itself never contained one. (finding: raw/display whitespace mismatch)
        let raw_content = "\tneedle";
        let text = Text::from(vec![Line::from(Span::raw("   1 │  needle"))]);

        let highlighted = highlight_full_file_text(text, Some(" "), raw_content, 0);

        assert!(highlighted.lines[0]
            .spans
            .iter()
            .all(|s| s.style.bg.is_none()));
    }

    #[test]
    fn highlight_full_file_text_does_not_highlight_the_cursor_rows_trailing_pane_padding() {
        // The raw line is just "needle" with no trailing space, but a cursor/tint row is
        // padded with artificial spaces out to the pane width (tint_line_bg). A query that
        // would only match by reaching into that padding must not highlight anything.
        let raw_content = "needle";
        let text = Text::from(vec![Line::from(Span::raw("   1 │ needle          "))]);

        let highlighted = highlight_full_file_text(text, Some("e "), raw_content, 0);

        assert!(highlighted.lines[0]
            .spans
            .iter()
            .all(|s| s.style.bg.is_none()));
    }

    #[test]
    fn highlight_full_file_text_shifts_the_match_correctly_past_a_multibyte_prefix() {
        // A multi-byte UTF-8 character before the match must not throw off the byte-offset
        // shift from raw content into display coordinates.
        let raw_content = "日本語 needle";
        let text = Text::from(vec![Line::from(Span::raw("   1 │ 日本語 needle"))]);

        let highlighted = highlight_full_file_text(text, Some("needle"), raw_content, 0);

        assert!(highlighted.lines[0]
            .spans
            .iter()
            .any(|s| s.style.bg == Some(Color::Yellow) && s.content.as_ref() == "needle"));
    }

    #[test]
    fn highlight_full_file_text_handles_an_empty_raw_line_without_panicking() {
        let raw_content = "\nneedle";
        let text = Text::from(vec![
            Line::from(Span::raw("   1 │ ")),
            Line::from(Span::raw("   2 │ needle")),
        ]);

        let highlighted = highlight_full_file_text(text, Some("needle"), raw_content, 0);

        assert!(highlighted.lines[0]
            .spans
            .iter()
            .all(|s| s.style.bg.is_none()));
        assert!(highlighted.lines[1]
            .spans
            .iter()
            .any(|s| s.style.bg == Some(Color::Yellow) && s.content.as_ref() == "needle"));
    }

    #[test]
    fn highlight_full_file_text_ignores_rows_past_the_end_of_raw_content() {
        // A bottom border row after the last content row has no corresponding raw line —
        // it must be skipped, not panic on an out-of-range index.
        let raw_content = "needle";
        let text = Text::from(vec![
            Line::from(Span::raw("   1 │ needle")),
            Line::from(Span::raw("──────────────")),
        ]);

        let highlighted = highlight_full_file_text(text, Some("needle"), raw_content, 0);

        assert!(highlighted.lines[1]
            .spans
            .iter()
            .all(|s| s.style.bg.is_none()));
    }
}
