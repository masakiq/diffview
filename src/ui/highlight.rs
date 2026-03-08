use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
};

fn search_highlight_style() -> Style {
    Style::default()
        .bg(Color::Yellow)
        .add_modifier(Modifier::BOLD)
}

fn match_ranges(text: &str, query: &str) -> Vec<(usize, usize)> {
    if query.is_empty() {
        return Vec::new();
    }

    let text_lower = text.to_lowercase();
    let query_lower = query.to_lowercase();
    let mut ranges = Vec::new();
    let mut offset = 0;

    while let Some(found) = text_lower[offset..].find(&query_lower) {
        let start = offset + found;
        let end = start + query_lower.len();
        ranges.push((start, end));
        offset = end;
    }

    ranges
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

        for &(match_start, match_end) in &ranges {
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
}
