//! Where each byte of the SQL text comes from: a piece of host text (byte for byte), an escape or a
//! placeholder of the host (all of it at once), a hole, or text this crate wrote around the
//! fragment, which has no place in the host at all.

use crate::fragment::{EscapeStyle, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    Synthetic,
    /// The SQL bytes are the host's from `host` on.
    Text {
        host: u32,
        run: u32,
    },
    /// The SQL bytes stand for `host` as a whole: an escape sequence, or a placeholder written
    /// differently in SQL.
    Atomic {
        host: Span,
        run: u32,
    },
    Hole {
        host: Span,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Segment {
    pub sql: Span,
    pub origin: Origin,
}

impl Segment {
    fn host_start(&self) -> Option<u32> {
        match self.origin {
            Origin::Synthetic => None,
            Origin::Text { host, .. } => Some(host),
            Origin::Atomic { host, .. } | Origin::Hole { host } => Some(host.start),
        }
    }

    fn host_end(&self) -> Option<u32> {
        match self.origin {
            Origin::Synthetic => None,
            Origin::Text { host, .. } => Some(host + self.sql.len()),
            Origin::Atomic { host, .. } | Origin::Hole { host } => Some(host.end),
        }
    }

    fn run(&self) -> Option<u32> {
        match self.origin {
            Origin::Text { run, .. } | Origin::Atomic { run, .. } => Some(run),
            _ => None,
        }
    }

    fn is_text(&self) -> bool {
        self.run().is_some()
    }
}

/// Which side of a boundary between two segments an offset belongs to: an end of a range belongs
/// to what comes before it, a start to what comes after.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Bias {
    Left,
    Right,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SourceMap {
    segments: Vec<Segment>,
    /// The style of each run: a stretch of one host literal, contiguous in the host.
    styles: Vec<EscapeStyle>,
    /// The SQL of the fragment's own pieces, without what was written around them.
    pub body: Span,
}

impl SourceMap {
    pub fn holes(&self) -> impl Iterator<Item = Span> + '_ {
        self.segments
            .iter()
            .filter(|segment| matches!(segment.origin, Origin::Hole { .. }))
            .map(|segment| segment.sql)
    }

    /// Whether a span of SQL touches a hole, at its edges included.
    pub fn touches_hole(&self, span: Span) -> bool {
        self.holes()
            .any(|hole| hole.start <= span.end && span.start <= hole.end)
    }

    /// The host offset of a SQL offset.
    pub fn to_host(&self, offset: u32, bias: Bias) -> Option<u32> {
        if let Some(inside) = self
            .segments
            .iter()
            .find(|segment| segment.sql.start < offset && offset < segment.sql.end)
        {
            return match inside.origin {
                Origin::Synthetic => None,
                Origin::Text { host, .. } => Some(host + (offset - inside.sql.start)),
                Origin::Atomic { host, .. } | Origin::Hole { host } => Some(match bias {
                    Bias::Left => host.end,
                    Bias::Right => host.start,
                }),
            };
        }
        let left = self.segments.iter().rev().find(|segment| segment.sql.end == offset);
        let right = self.segments.iter().find(|segment| segment.sql.start == offset);
        let left = left.and_then(Segment::host_end);
        let right = right.and_then(Segment::host_start);
        match bias {
            Bias::Left => left.or(right),
            Bias::Right => right.or(left),
        }
    }

    /// The SQL offset of a host offset in the text of a piece: a cursor. An offset inside a hole
    /// or between pieces has none.
    pub fn to_sql(&self, host: u32) -> Option<u32> {
        let text = self.segments.iter().find_map(|segment| match segment.origin {
            Origin::Text { host: start, .. } if start <= host && host <= start + segment.sql.len() => {
                Some(segment.sql.start + (host - start))
            }
            _ => None,
        });
        text.or_else(|| {
            self.segments.iter().find_map(|segment| match segment.origin {
                Origin::Atomic { host: span, .. } if span.touches(host) => Some(if host == span.end {
                    segment.sql.end
                } else {
                    segment.sql.start
                }),
                _ => None,
            })
        })
    }

    /// The host span of a span of the fragment's own SQL, or nothing when it reaches into what was
    /// written around the fragment.
    pub fn range(&self, span: Span) -> Option<Span> {
        if span.start < self.body.start || span.end > self.body.end || span.start > span.end {
            return None;
        }
        if span.is_empty() {
            return self.to_host(span.start, Bias::Left).map(Span::empty);
        }
        let start = self.to_host(span.start, Bias::Right)?;
        let end = self.to_host(span.end, Bias::Left)?;
        Some(if start <= end {
            Span::new(start, end)
        } else {
            Span::empty(start)
        })
    }

    /// The host spans of the text a span of SQL covers, one per stretch that is contiguous in the
    /// host, without holes.
    pub fn pieces(&self, span: Span) -> Vec<Span> {
        let mut out: Vec<Span> = Vec::new();
        for segment in &self.segments {
            let start = span.start.max(segment.sql.start);
            let end = span.end.min(segment.sql.end);
            if start >= end {
                continue;
            }
            let host = match segment.origin {
                Origin::Text { host, .. } => {
                    Span::new(host + (start - segment.sql.start), host + (end - segment.sql.start))
                }
                Origin::Atomic { host, .. } => host,
                Origin::Synthetic | Origin::Hole { .. } => continue,
            };
            match out.last_mut() {
                Some(last) if last.end == host.start => last.end = host.end,
                _ => out.push(host),
            }
        }
        out
    }

    /// Where an edit of a span of SQL goes in the host, and how its text is escaped there: only
    /// within one stretch of one host literal, never over a hole, between two literals or into
    /// what was written around the fragment, and never through part of an escape.
    pub fn edit(&self, span: Span) -> Option<(Span, EscapeStyle)> {
        if span.start > span.end {
            return None;
        }
        if span.is_empty() {
            let offset = span.start;
            if self
                .segments
                .iter()
                .any(|segment| segment.sql.start < offset && offset < segment.sql.end && !segment.is_text())
            {
                return None;
            }
            if let Some(inside) = self
                .segments
                .iter()
                .find(|segment| segment.sql.start < offset && offset < segment.sql.end)
            {
                let Origin::Text { host, run } = inside.origin else {
                    return None;
                };
                let at = host + (offset - inside.sql.start);
                return Some((Span::empty(at), self.styles[run as usize]));
            }
            let left = self
                .segments
                .iter()
                .rev()
                .find(|segment| segment.sql.end == offset && segment.is_text());
            let right = self
                .segments
                .iter()
                .find(|segment| segment.sql.start == offset && segment.is_text());
            let (at, run) = match (left, right) {
                (Some(left), _) => (left.host_end()?, left.run()?),
                (None, Some(right)) => (right.host_start()?, right.run()?),
                (None, None) => return None,
            };
            return Some((Span::empty(at), self.styles[run as usize]));
        }
        let touched: Vec<&Segment> = self
            .segments
            .iter()
            .filter(|segment| segment.sql.start < span.end && span.start < segment.sql.end)
            .collect();
        let first = touched.first()?;
        let run = first.run()?;
        for segment in &touched {
            if segment.run() != Some(run) {
                return None;
            }
            if matches!(segment.origin, Origin::Atomic { .. })
                && (segment.sql.start < span.start || span.end < segment.sql.end)
            {
                return None;
            }
        }
        let last = touched.last()?;
        let start = match first.origin {
            Origin::Text { host, .. } => host + (span.start - first.sql.start),
            _ => first.host_start()?,
        };
        let end = match last.origin {
            Origin::Text { host, .. } => host + (span.end - last.sql.start),
            _ => last.host_end()?,
        };
        Some((Span::new(start, end), self.styles[run as usize]))
    }
}

/// Writes the SQL text and its map side by side.
#[derive(Default)]
pub(crate) struct Builder {
    pub text: String,
    map: SourceMap,
    /// The run the last segment was part of, its host end and style, while the next piece may
    /// continue it.
    open_run: Option<(u32, u32, EscapeStyle)>,
}

impl Builder {
    pub fn offset(&self) -> u32 {
        self.text.len() as u32
    }

    fn push(&mut self, text: &str, origin: Origin) {
        let start = self.offset();
        self.text.push_str(text);
        self.map.segments.push(Segment {
            sql: Span::new(start, self.offset()),
            origin,
        });
    }

    fn run_for(&mut self, host_start: u32, host_end: u32, style: EscapeStyle) -> u32 {
        let run = match self.open_run {
            Some((run, end, open_style)) if end == host_start && open_style == style => run,
            _ => {
                self.map.styles.push(style);
                self.map.styles.len() as u32 - 1
            }
        };
        self.open_run = Some((run, host_end, style));
        run
    }

    pub fn synthetic(&mut self, text: &str) {
        self.open_run = None;
        self.push(text, Origin::Synthetic);
    }

    pub fn text(&mut self, text: &str, host: u32, style: EscapeStyle) {
        let run = self.run_for(host, host + text.len() as u32, style);
        self.push(text, Origin::Text { host, run });
    }

    pub fn atomic(&mut self, text: &str, host: Span, style: EscapeStyle) {
        let run = self.run_for(host.start, host.end, style);
        self.push(text, Origin::Atomic { host, run });
    }

    pub fn hole(&mut self, text: &str, host: Span) {
        self.open_run = None;
        self.push(text, Origin::Hole { host });
    }

    pub fn finish(mut self, body: Span) -> (String, SourceMap) {
        self.map.body = body;
        (self.text, self.map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `'SELECT * FROM t WHERE a = \'x\'' . $b` with the string at 10.
    fn sample() -> (String, SourceMap) {
        let mut builder = Builder::default();
        builder.synthetic("PRE ");
        let start = builder.offset();
        builder.text("SELECT * FROM t WHERE a = ", 11, EscapeStyle::SingleQuoted);
        builder.atomic("'", Span::new(37, 39), EscapeStyle::SingleQuoted);
        builder.text("x", 39, EscapeStyle::SingleQuoted);
        builder.atomic("'", Span::new(40, 42), EscapeStyle::SingleQuoted);
        builder.hole("?", Span::new(46, 48));
        let end = builder.offset();
        builder.synthetic(" POST");
        builder.finish(Span::new(start, end))
    }

    #[test]
    fn maps_offsets_both_ways() {
        let (text, map) = sample();
        let sql = |needle: &str| text.find(needle).unwrap() as u32;
        assert_eq!(map.to_host(sql("FROM"), Bias::Right), Some(20));
        assert_eq!(map.to_sql(20), Some(sql("FROM")));
        assert_eq!(map.to_host(sql("x"), Bias::Right), Some(39));
        assert_eq!(map.to_host(sql("x"), Bias::Left), Some(39), "a boundary goes left");
        assert_eq!(
            map.to_host(0, Bias::Right),
            None,
            "what was written around has no place"
        );
        assert_eq!(map.to_sql(38), Some(sql("'x")), "inside an escape is its start");
        assert_eq!(map.to_sql(47), None, "inside a hole is the host's");
        let string = Span::new(sql("'x"), sql("'x") + 3);
        assert_eq!(map.range(string), Some(Span::new(37, 42)));
        assert_eq!(map.pieces(string), [Span::new(37, 42)]);
        assert_eq!(map.range(Span::new(0, 3)), None);
    }

    #[test]
    fn edits_stay_inside_one_literal() {
        let (text, map) = sample();
        let sql = |needle: &str| text.find(needle).unwrap() as u32;
        let from = Span::new(sql("FROM"), sql("FROM") + 4);
        assert_eq!(map.edit(from), Some((Span::new(20, 24), EscapeStyle::SingleQuoted)));
        let string = Span::new(sql("'x"), sql("'x") + 3);
        assert_eq!(map.edit(string), Some((Span::new(37, 42), EscapeStyle::SingleQuoted)));
        let with_escape = Span::new(sql("'x") + 1, sql("'x") + 3);
        assert_eq!(
            map.edit(with_escape),
            Some((Span::new(39, 42), EscapeStyle::SingleQuoted))
        );
        let over_hole = Span::new(sql("'x"), sql("?") + 1);
        assert_eq!(map.edit(over_hole), None);
        assert_eq!(
            map.edit(Span::empty(sql("?"))),
            Some((Span::empty(42), EscapeStyle::SingleQuoted)),
            "an insertion before a hole goes at the end of the text"
        );
        assert_eq!(
            map.edit(Span::empty(4)),
            Some((Span::empty(11), EscapeStyle::SingleQuoted))
        );
    }
}
