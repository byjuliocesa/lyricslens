//! The clock that says where the song is between readings.
//!
//! Browsers report `Position` rounded down to whole seconds, so a reading
//! carries up to a second of error and is useless on its own. The moment the
//! integer flips, though, the real position is known exactly — that instant is
//! the anchor, and from there time is counted locally.
//!
//! `Position` has no change signal by specification, so it has to be read on a
//! timer. Everything here is driven by those readings and stays testable
//! without a bus.

use std::time::{Duration, Instant};

use crate::sync::Playback;

/// Beyond this, the reading and the estimate are not describing the same play.
const SEEK: Duration = Duration::from_millis(250);

/// The width of one quantised reading.
const STEP: Duration = Duration::from_secs(1);

/// What a reading turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reading {
    /// The integer flipped: the position is known exactly, and time restarts here.
    Edge,
    /// The position jumped. Someone dragged the bar, or the track changed.
    Seek,
    /// Nothing new; the estimate still holds.
    Steady,
}

#[derive(Debug, Clone, Copy)]
struct Anchor {
    at: Instant,
    position: Duration,
}

#[derive(Debug)]
pub struct Clock {
    anchor: Option<Anchor>,
    last_reading: Option<Duration>,
    state: Playback,
    /// Where the song stopped, while it is not playing.
    frozen: Option<Duration>,
    /// Manual correction for this player, in milliseconds. Positive means the
    /// lyrics run early and have to wait.
    offset_ms: i64,
}

impl Clock {
    pub fn new(offset_ms: i64) -> Self {
        Self {
            anchor: None,
            last_reading: None,
            state: Playback::Stopped,
            frozen: None,
            offset_ms,
        }
    }

    pub fn set_offset_ms(&mut self, offset_ms: i64) {
        self.offset_ms = offset_ms;
    }

    /// Forgets the song. Called when the track changes.
    pub fn reset(&mut self) {
        self.anchor = None;
        self.last_reading = None;
        self.frozen = None;
    }

    /// Follows the player between playing, paused and stopped.
    pub fn playback(&mut self, state: Playback, now: Instant) {
        if state == self.state {
            return;
        }

        match state {
            // Freeze where the song stands, so a pause does not let the
            // estimate run on without it.
            Playback::Paused => self.frozen = self.raw_position(now),
            Playback::Playing => {
                if let Some(position) = self.frozen.take() {
                    self.anchor = Some(Anchor { at: now, position });
                }
            }
            Playback::Stopped => self.reset(),
        }
        self.state = state;
    }

    /// Feeds one `Position` reading and says what it meant.
    pub fn sample(&mut self, reading: Duration, now: Instant) -> Reading {
        // A player that reports sub-second detail is not guessing: take it.
        let exact = reading.subsec_millis() != 0;

        let Some(anchor) = self.anchor else {
            self.set(reading, now);
            return Reading::Edge;
        };

        let estimate = anchor.position + now.saturating_duration_since(anchor.at);

        if exact {
            let jumped = estimate.abs_diff(reading) > SEEK;
            self.set(reading, now);
            return if jumped { Reading::Seek } else { Reading::Edge };
        }

        match self.last_reading {
            // The integer flipped. One step forward is the song playing; any
            // other jump is someone moving the bar.
            Some(last) if last != reading => {
                let expected = last + STEP;
                self.set(reading, now);
                if reading == expected {
                    Reading::Edge
                } else {
                    Reading::Seek
                }
            }
            // Same second as before. The estimate has to sit inside the second
            // that reading stands for, or the two are describing different plays.
            _ => {
                if estimate + SEEK < reading || estimate > reading + STEP + SEEK {
                    self.set(reading, now);
                    Reading::Seek
                } else {
                    Reading::Steady
                }
            }
        }
    }

    /// Where the song is, with the manual offset applied.
    pub fn position(&self, now: Instant) -> Option<Duration> {
        let position = self.raw_position(now)?;
        Some(shift(position, self.offset_ms))
    }

    /// Where the song is as the player has it, before the manual offset.
    pub fn raw_position(&self, now: Instant) -> Option<Duration> {
        if self.state != Playback::Playing {
            return self.frozen;
        }
        let anchor = self.anchor?;
        Some(anchor.position + now.saturating_duration_since(anchor.at))
    }

    fn set(&mut self, reading: Duration, now: Instant) {
        self.anchor = Some(Anchor {
            at: now,
            position: reading,
        });
        self.last_reading = Some(reading);
        self.frozen = None;
    }
}

/// A positive offset means the lyrics arrive early, so the position moves back.
fn shift(position: Duration, offset_ms: i64) -> Duration {
    let offset = Duration::from_millis(offset_ms.unsigned_abs());
    if offset_ms >= 0 {
        position.saturating_sub(offset)
    } else {
        position + offset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// A clock already playing, anchored at `position`.
    fn playing(base: Instant, position: Duration) -> Clock {
        let mut clock = Clock::new(0);
        clock.playback(Playback::Playing, base);
        clock.sample(position, base);
        clock
    }

    #[test]
    fn the_first_reading_anchors_the_clock() {
        let base = Instant::now();
        let mut clock = Clock::new(0);
        clock.playback(Playback::Playing, base);

        assert_eq!(clock.sample(secs(10), base), Reading::Edge);
        assert_eq!(clock.position(base), Some(secs(10)));
    }

    #[test]
    fn time_runs_on_between_readings() {
        let base = Instant::now();
        let clock = playing(base, secs(10));
        assert_eq!(clock.position(base + ms(400)), Some(ms(10_400)));
    }

    #[test]
    fn the_same_second_read_again_changes_nothing() {
        let base = Instant::now();
        let mut clock = playing(base, secs(10));

        assert_eq!(clock.sample(secs(10), base + ms(300)), Reading::Steady);
        assert_eq!(clock.position(base + ms(300)), Some(ms(10_300)));
    }

    #[test]
    fn the_flip_to_the_next_second_re_anchors() {
        let base = Instant::now();
        let mut clock = playing(base, secs(10));

        // The player was read late, so the local estimate had drifted ahead.
        assert_eq!(clock.sample(secs(11), base + ms(1_060)), Reading::Edge);
        assert_eq!(clock.position(base + ms(1_060)), Some(secs(11)));
    }

    #[test]
    fn a_jump_forward_is_a_seek() {
        let base = Instant::now();
        let mut clock = playing(base, secs(10));

        assert_eq!(clock.sample(secs(95), base + ms(500)), Reading::Seek);
        assert_eq!(clock.position(base + ms(500)), Some(secs(95)));
    }

    #[test]
    fn a_jump_backwards_is_a_seek_too() {
        let base = Instant::now();
        let mut clock = playing(base, secs(60));

        assert_eq!(clock.sample(secs(5), base + ms(500)), Reading::Seek);
        assert_eq!(clock.position(base + ms(500)), Some(secs(5)));
    }

    #[test]
    fn an_estimate_that_outran_its_second_is_a_seek() {
        let base = Instant::now();
        let mut clock = playing(base, secs(10));

        // Two seconds later the player still says 10: it was moved back, and
        // no flip will ever announce it.
        assert_eq!(clock.sample(secs(10), base + ms(2_000)), Reading::Seek);
    }

    #[test]
    fn a_player_that_reports_sub_second_detail_is_believed() {
        let base = Instant::now();
        let mut clock = Clock::new(0);
        clock.playback(Playback::Playing, base);

        assert_eq!(clock.sample(ms(10_137), base), Reading::Edge);
        assert_eq!(clock.position(base + ms(100)), Some(ms(10_237)));
        assert_eq!(clock.sample(ms(10_240), base + ms(100)), Reading::Edge);
    }

    #[test]
    fn a_sub_second_reading_far_from_the_estimate_is_a_seek() {
        let base = Instant::now();
        let mut clock = Clock::new(0);
        clock.playback(Playback::Playing, base);
        clock.sample(ms(10_137), base);

        assert_eq!(clock.sample(ms(80_500), base + ms(100)), Reading::Seek);
    }

    #[test]
    fn a_pause_freezes_the_position() {
        let base = Instant::now();
        let mut clock = playing(base, secs(10));

        clock.playback(Playback::Paused, base + ms(400));
        assert_eq!(clock.position(base + ms(400)), Some(ms(10_400)));
        // Ten seconds of staring at a paused player change nothing.
        assert_eq!(clock.position(base + secs(10)), Some(ms(10_400)));
    }

    #[test]
    fn resuming_counts_from_where_it_stopped() {
        let base = Instant::now();
        let mut clock = playing(base, secs(10));
        clock.playback(Playback::Paused, base + ms(400));

        clock.playback(Playback::Playing, base + secs(30));
        assert_eq!(clock.position(base + secs(30) + ms(100)), Some(ms(10_500)));
    }

    #[test]
    fn stopping_forgets_the_song() {
        let base = Instant::now();
        let mut clock = playing(base, secs(10));

        clock.playback(Playback::Stopped, base + ms(400));
        assert_eq!(clock.position(base + ms(400)), None);
    }

    #[test]
    fn a_positive_offset_holds_the_lyrics_back() {
        let base = Instant::now();
        let mut clock = playing(base, secs(10));

        clock.set_offset_ms(500);
        assert_eq!(clock.position(base), Some(ms(9_500)));
        clock.set_offset_ms(-500);
        assert_eq!(clock.position(base), Some(ms(10_500)));
    }

    #[test]
    fn a_track_change_leaves_nothing_behind() {
        let base = Instant::now();
        let mut clock = playing(base, secs(10));

        clock.reset();
        assert_eq!(clock.position(base), None);
        assert_eq!(clock.sample(secs(0), base), Reading::Edge);
    }
}
