//! Lyrics: where they come from, and how they are read.

pub mod lrc;
pub mod lrclib;
pub mod normalize;

use std::time::Duration;

/// One line of a song, anchored at the moment it should appear.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub at: Duration,
    pub kind: LineKind,
}

/// A blank line in an LRC file is not a line with no words: it is the moment
/// the screen has to go quiet. Spelling that out keeps the overlay from
/// leaving the last line frozen through a whole instrumental.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineKind {
    Sung(String),
    Instrumental,
}

impl Line {
    /// The words to show, or `None` while nothing is being sung.
    pub fn sung(&self) -> Option<&str> {
        match &self.kind {
            LineKind::Sung(text) => Some(text),
            LineKind::Instrumental => None,
        }
    }
}

/// A whole song, in the order it is sung.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lyrics {
    pub lines: Vec<Line>,
    /// What the file's `[offset:...]` tag asks for, in milliseconds. Positive
    /// means the lyrics run early and have to be pushed later.
    pub offset_ms: i64,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
}

impl Lyrics {
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The line being sung at `position`, or `None` before the first one.
    ///
    /// An instrumental gap is a line like any other here: it is found, and it
    /// has no words, which is how the screen goes quiet.
    pub fn line_at(&self, position: Duration) -> Option<&Line> {
        self.index_at(position).map(|index| &self.lines[index])
    }

    /// Where in the song `position` falls, as a place in `lines`.
    ///
    /// The window that lists the whole song needs the place, not the line:
    /// two identical lines are two different rows on screen.
    pub fn index_at(&self, position: Duration) -> Option<usize> {
        let position = self.shifted(position);
        // The last line whose moment has already passed.
        let index = self.lines.partition_point(|line| line.at <= position);
        index.checked_sub(1)
    }

    /// The line sung just before this one, skipping the silences.
    pub fn before(&self, position: Duration) -> Option<&str> {
        let position = self.shifted(position);
        let index = self.lines.partition_point(|line| line.at <= position);
        self.lines[..index.checked_sub(1)?]
            .iter()
            .rev()
            .find_map(|line| line.sung())
    }

    /// How far through the current line the song is, from 0 to 1.
    ///
    /// Lines carry one timestamp, not one per word, so this is the share of
    /// the time between this line and the next — close enough to follow, and
    /// the only thing the data supports.
    pub fn progress_at(&self, position: Duration) -> Option<f64> {
        let position = self.shifted(position);
        let index = self.lines.partition_point(|line| line.at <= position);
        let current = self.lines.get(index.checked_sub(1)?)?;

        let ends = match self.lines.get(index) {
            Some(next) => next.at,
            // The last line has no end; hold it full rather than guessing.
            None => return Some(1.0),
        };
        let span = ends.checked_sub(current.at)?.as_secs_f64();
        if span <= 0.0 {
            return Some(1.0);
        }
        Some(((position - current.at).as_secs_f64() / span).clamp(0.0, 1.0))
    }

    /// The next lines to be sung, skipping the silences.
    ///
    /// An instrumental gap is left out on purpose: showing a blank line in a
    /// list of what is coming says nothing.
    pub fn after(&self, position: Duration, how_many: usize) -> Vec<String> {
        let position = self.shifted(position);
        let start = self.lines.partition_point(|line| line.at <= position);
        self.lines[start..]
            .iter()
            .filter_map(|line| line.sung())
            .take(how_many)
            .map(str::to_owned)
            .collect()
    }

    /// The correction, in milliseconds, that makes line `index` start at
    /// `reading`: the position as the player reports it, before any
    /// correction.
    ///
    /// The result is in the clock's terms, where a positive correction holds
    /// the lyrics back. The file's own `[offset:]` stays applied on top, so it
    /// is taken out here.
    pub fn correction_to_start(&self, index: usize, reading: Duration) -> Option<i64> {
        let line = self.lines.get(index)?;
        let starts = i64::try_from(line.at.as_millis()).ok()? - self.offset_ms;
        Some(i64::try_from(reading.as_millis()).ok()? - starts)
    }

    /// The sung line whose start is the smallest change away from the
    /// correction in force, for when someone says "this line starts now"
    /// without saying which.
    ///
    /// The line they mean is the one nearest to where the lyrics think the
    /// song is, so it is right while the lyrics are off by less than half the
    /// gap between two lines.
    pub fn nearest_line(&self, reading: Duration, correction_ms: i64) -> Option<usize> {
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.sung().is_some())
            .filter_map(|(index, _)| {
                let needed = self.correction_to_start(index, reading)?;
                Some((index, needed.abs_diff(correction_ms)))
            })
            .min_by_key(|(_, change)| *change)
            .map(|(index, _)| index)
    }

    /// The file's own `[offset:]`, applied to a position before looking it up.
    fn shifted(&self, position: Duration) -> Duration {
        let offset = Duration::from_millis(self.offset_ms.unsigned_abs());
        if self.offset_ms >= 0 {
            position + offset
        } else {
            position.saturating_sub(offset)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song() -> Lyrics {
        lrc::parse("[00:10.00]first\n[00:20.00]\n[00:30.00]second")
    }

    #[test]
    fn nothing_is_sung_before_the_first_line() {
        assert!(song().line_at(Duration::from_secs(5)).is_none());
    }

    #[test]
    fn a_line_holds_until_the_next_one() {
        let song = song();
        assert_eq!(
            song.line_at(Duration::from_secs(10)).unwrap().sung(),
            Some("first")
        );
        assert_eq!(
            song.line_at(Duration::from_secs(19)).unwrap().sung(),
            Some("first")
        );
    }

    #[test]
    fn a_gap_clears_the_screen() {
        assert_eq!(
            song().line_at(Duration::from_secs(25)).unwrap().sung(),
            None
        );
    }

    #[test]
    fn the_last_line_stays_to_the_end() {
        assert_eq!(
            song().line_at(Duration::from_secs(600)).unwrap().sung(),
            Some("second")
        );
    }

    #[test]
    fn the_files_own_offset_moves_the_lookup() {
        let mut song = song();
        // The file says its lyrics run half a second late.
        song.offset_ms = 500;
        assert_eq!(
            song.line_at(Duration::from_millis(9_600)).unwrap().sung(),
            Some("first")
        );
    }

    #[test]
    fn the_lines_still_to_come_skip_the_silences() {
        let song = song();
        assert_eq!(song.after(Duration::from_secs(0), 2), ["first", "second"]);
        assert_eq!(song.after(Duration::from_secs(12), 2), ["second"]);
        assert!(song.after(Duration::from_secs(600), 2).is_empty());
    }

    #[test]
    fn progress_runs_from_the_line_to_the_next() {
        let song = song();
        assert_eq!(song.progress_at(Duration::from_secs(10)), Some(0.0));
        assert_eq!(song.progress_at(Duration::from_secs(15)), Some(0.5));
        assert_eq!(song.progress_at(Duration::from_secs(5)), None);
        // The last line has no next one to measure against.
        assert_eq!(song.progress_at(Duration::from_secs(600)), Some(1.0));
    }

    #[test]
    fn the_line_before_skips_the_silences() {
        let song = song();
        assert_eq!(song.before(Duration::from_secs(35)), Some("first"));
        // Nothing has been sung yet at the very start.
        assert_eq!(song.before(Duration::from_secs(12)), None);
        assert_eq!(song.before(Duration::from_secs(5)), None);
    }

    #[test]
    fn an_empty_song_has_no_lines() {
        assert!(Lyrics::default().line_at(Duration::from_secs(1)).is_none());
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_line_starting_now_needs_the_gap_between_it_and_the_player() {
        let song = lrc::parse("[00:10.00]first\n[00:20.00]second");
        // The player says 12.5 s as "first" begins: the lyrics are 2.5 s early
        // and have to be held back that much.
        assert_eq!(song.correction_to_start(0, ms(12_500)), Some(2_500));
        // Said of "second" at 18 s, they are late and come forward.
        assert_eq!(song.correction_to_start(1, ms(18_000)), Some(-2_000));
        assert_eq!(song.correction_to_start(5, ms(18_000)), None);
    }

    #[test]
    fn the_files_own_offset_is_taken_out() {
        let mut song = lrc::parse("[00:10.00]first");
        song.offset_ms = 500;
        // The file already moves the line to 9.5 s on the clock.
        assert_eq!(song.correction_to_start(0, ms(10_000)), Some(500));
    }

    #[test]
    fn the_nearest_line_is_the_smallest_change() {
        let song = lrc::parse("[00:10.00]first\n[00:15.00]\n[00:20.00]second");
        assert_eq!(song.nearest_line(ms(11_000), 0), Some(0));
        assert_eq!(song.nearest_line(ms(19_000), 0), Some(2));
        // With 9 s already in force, 20 s on the player is "first" again.
        assert_eq!(song.nearest_line(ms(20_000), 9_000), Some(0));
        // The instrumental gap is never the answer.
        assert_eq!(song.nearest_line(ms(15_000), 0), Some(0));
    }
}
