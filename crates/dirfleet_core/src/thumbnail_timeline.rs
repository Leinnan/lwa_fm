//! Which frames of a video the thumbnail picker offers, and in what order they
//! are extracted.
//!
//! A thumbnail usually comes from the start of a video, so the
//! positions crowd towards it and the earliest are extracted first. Any gap
//! between two frames can then be filled with more of them.
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

/// Frames offered before the user asks for more.
const INITIAL_FRAMES: u32 = 16;
/// New frames per filled gap.
const FILL_FRAMES: u64 = 4;
/// Frames are never closer than this, or than one video frame.
const MIN_GAP_MS: u64 = 100;
const MAX_FRAMES: usize = 64;
const MAX_BYTES: usize = 48 * 1024 * 1024;

/// The closest two frames may be in a video with this frame rate.
fn min_gap_ms(frame_rate: Option<f64>) -> u64 {
    let frame = frame_rate
        .filter(|rate| *rate > 0.0)
        .map_or(0, |rate| (1000.0 / rate).ceil() as u64);
    frame.max(MIN_GAP_MS)
}

/// The first frames to offer for a video of `duration` seconds, in ascending
/// order. Quadratic spacing crowds them towards the start while still reaching
/// the end. Without a usable duration, only the first frame can be offered.
fn initial_times(duration: Option<f64>, min_gap_ms: u64) -> Vec<u64> {
    let Some(duration) = duration.filter(|duration| duration.is_finite() && *duration > 0.0) else {
        return vec![0];
    };
    let start = (duration * 0.01).min(0.5);
    let end = duration * 0.95;
    let mut times: Vec<u64> = Vec::with_capacity(INITIAL_FRAMES as usize);
    for index in 0..INITIAL_FRAMES {
        let progress = f64::from(index) / f64::from(INITIAL_FRAMES - 1);
        let ms = ((end - start).mul_add(progress * progress, start) * 1000.0) as u64;
        if times.last().is_none_or(|last| ms >= last + min_gap_ms) {
            times.push(ms);
        }
    }
    times
}

/// Evenly spaced positions strictly between `from` and `to`; none when the gap
/// is too small to hold a frame that is `min_gap_ms` from both ends.
fn gap_times(from: u64, to: u64, min_gap_ms: u64) -> Vec<u64> {
    let gap = to.saturating_sub(from);
    let count = (gap / min_gap_ms).saturating_sub(1).min(FILL_FRAMES);
    (1..=count)
        .map(|index| from + gap * index / (count + 1))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Slot<T> {
    /// Waiting for its turn.
    Queued,
    /// Being extracted.
    Loading,
    Ready(T),
}

/// A stretch of the video that can be filled with more frames, in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    pub from: u64,
    pub to: u64,
}

/// The frames of one picker session by position in milliseconds, and the work
/// left to do. `T` is whatever a finished frame is drawn with.
#[derive(Debug)]
pub struct FrameTimeline<T> {
    slots: BTreeMap<u64, Slot<T>>,
    queue: VecDeque<u64>,
    /// The last position a frame can be asked for, once the length is known.
    end_ms: Option<u64>,
    default_ms: Option<u64>,
    min_gap_ms: u64,
    bytes: usize,
    failed: usize,
}

impl<T> Default for FrameTimeline<T> {
    fn default() -> Self {
        Self {
            slots: BTreeMap::new(),
            queue: VecDeque::new(),
            end_ms: None,
            default_ms: None,
            min_gap_ms: MIN_GAP_MS,
            bytes: 0,
            failed: 0,
        }
    }
}

impl<T> FrameTimeline<T> {
    /// Lays out the first frames. The chosen frame (or else the default one)
    /// is extracted first, so the large preview fills straight away.
    pub fn start(
        &mut self,
        duration: Option<f64>,
        frame_rate: Option<f64>,
        default_at: Duration,
        current_at: Option<Duration>,
    ) {
        self.min_gap_ms = min_gap_ms(frame_rate);
        let known = duration.filter(|duration| duration.is_finite() && *duration > 0.0);
        self.end_ms =
            known.map(|duration| ((duration * 1000.0) as u64).saturating_sub(self.min_gap_ms));
        let default_ms = default_at.as_millis() as u64;
        let current_ms = current_at.map(|at| at.as_millis() as u64);
        self.default_ms = Some(default_ms);
        if let Some(current_ms) = current_ms {
            self.add_queued(current_ms);
        }
        self.add_queued(default_ms);
        // Both named frames stay exactly where they are, so the schedule makes
        // way for them.
        for ms in initial_times(known, self.min_gap_ms) {
            let crowded = [Some(default_ms), current_ms]
                .iter()
                .flatten()
                .any(|named| named.abs_diff(ms) < self.min_gap_ms);
            if !crowded {
                self.add_queued(ms);
            }
        }
    }

    fn add_queued(&mut self, ms: u64) {
        if let Entry::Vacant(slot) = self.slots.entry(ms) {
            slot.insert(Slot::Queued);
            self.queue.push_back(ms);
        }
    }

    /// Takes the next position to extract and marks it as being extracted.
    pub fn next_job(&mut self) -> Option<u64> {
        let ms = self.queue.pop_front()?;
        self.slots.insert(ms, Slot::Loading);
        Some(ms)
    }

    pub fn finish(&mut self, ms: u64, frame: T, bytes: usize) {
        if let Some(slot) = self.slots.get_mut(&ms) {
            *slot = Slot::Ready(frame);
            self.bytes = self.bytes.saturating_add(bytes);
        }
    }

    /// Removes a frame that could not be extracted.
    pub fn fail(&mut self, ms: u64) {
        if self.slots.remove(&ms).is_some() {
            self.failed += 1;
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (u64, &Slot<T>)> {
        self.slots.iter().map(|(ms, slot)| (*ms, slot))
    }

    pub fn frame(&self, ms: u64) -> Option<&T> {
        match self.slots.get(&ms) {
            Some(Slot::Ready(frame)) => Some(frame),
            _ => None,
        }
    }

    /// The nearest finished frame before (`forward` false) or after `ms`.
    pub fn ready_neighbor(&self, ms: u64, forward: bool) -> Option<u64> {
        if forward {
            self.slots
                .range(ms + 1..)
                .find_map(|(ms, slot)| matches!(slot, Slot::Ready(_)).then_some(*ms))
        } else {
            self.slots
                .range(..ms)
                .rev()
                .find_map(|(ms, slot)| matches!(slot, Slot::Ready(_)).then_some(*ms))
        }
    }

    /// Where the video's default thumbnail is; known after [`Self::start`].
    pub const fn default_ms(&self) -> Option<u64> {
        self.default_ms
    }

    /// The length of the video, once known.
    pub fn duration(&self) -> Option<Duration> {
        self.end_ms
            .map(|end| Duration::from_millis(end + self.min_gap_ms))
    }

    /// Frames extracted, and frames that will exist once the queue is done.
    pub fn progress(&self) -> (usize, usize) {
        let ready = self
            .slots
            .values()
            .filter(|slot| matches!(slot, Slot::Ready(_)))
            .count();
        (ready, self.slots.len())
    }

    /// Frames that could not be extracted.
    pub const fn failed(&self) -> usize {
        self.failed
    }

    /// Whether the session already holds as many frames as it may.
    pub fn at_limit(&self) -> bool {
        self.slots.len() >= MAX_FRAMES || self.bytes >= MAX_BYTES
    }

    fn gap(&self, from: u64, to: u64) -> Option<Gap> {
        (!self.at_limit() && !gap_times(from, to, self.min_gap_ms).is_empty())
            .then_some(Gap { from, to })
    }

    /// The fillable stretch after the frame at `ms`, up to the next frame or
    /// the end of the video.
    pub fn gap_after(&self, ms: u64) -> Option<Gap> {
        let to = self
            .slots
            .range(ms + 1..)
            .next()
            .map(|(next, _)| *next)
            .or(self.end_ms)?;
        self.gap(ms, to)
    }

    /// The fillable stretch before the frame at `ms`, back to the previous
    /// frame or the start of the video.
    pub fn gap_before(&self, ms: u64) -> Option<Gap> {
        let from = self
            .slots
            .range(..ms)
            .next_back()
            .map_or(0, |(previous, _)| *previous);
        self.gap(from, ms)
    }

    /// Adds frames across `gap`, extracted next and earliest first: they are
    /// what the user just asked for.
    pub fn fill(&mut self, gap: Gap) {
        let room = MAX_FRAMES.saturating_sub(self.slots.len());
        let times = gap_times(gap.from, gap.to, self.min_gap_ms);
        for ms in times.into_iter().take(room).rev() {
            if let Entry::Vacant(slot) = self.slots.entry(ms) {
                slot.insert(Slot::Queued);
                self.queue.push_front(ms);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: f64 = 3600.0;

    fn timeline(duration: f64) -> FrameTimeline<()> {
        let mut timeline = FrameTimeline::default();
        timeline.start(
            Some(duration),
            Some(30.0),
            Duration::from_secs_f64(duration * 0.05),
            None,
        );
        timeline
    }

    fn drain(timeline: &mut FrameTimeline<()>) -> Vec<u64> {
        std::iter::from_fn(|| timeline.next_job()).collect()
    }

    #[test]
    fn initial_times_crowd_the_start_and_reach_the_end() {
        let times = initial_times(Some(HOUR), 100);
        assert_eq!(times.len(), INITIAL_FRAMES as usize);
        assert!(times.is_sorted());
        assert_eq!(times[0], 500);
        assert_eq!(
            times[1], 15_697,
            "the second frame is seconds in, not minutes"
        );
        assert_eq!(*times.last().expect("frames"), 3_420_000);
        let first_half = times[7] - times[0];
        let second_half = times[15] - times[8];
        assert!(first_half * 2 < second_half);
    }

    #[test]
    fn initial_times_merge_on_short_clips() {
        let times = initial_times(Some(0.4), 100);
        assert!(times.len() < INITIAL_FRAMES as usize);
        assert!(times[0] < 100);
        assert!(times.windows(2).all(|pair| pair[1] - pair[0] >= 100));
        assert!(times.iter().all(|ms| *ms < 400));
    }

    #[test]
    fn unknown_duration_offers_only_the_first_frame() {
        assert_eq!(initial_times(None, 100), [0]);
        assert_eq!(initial_times(Some(f64::NAN), 100), [0]);
        assert_eq!(initial_times(Some(0.0), 100), [0]);
    }

    #[test]
    fn gap_times_are_even_strictly_between_and_stop_at_the_minimum() {
        assert_eq!(gap_times(0, 5_000, 100), [1_000, 2_000, 3_000, 4_000]);
        assert_eq!(gap_times(1_000, 1_300, 100), [1_100, 1_200]);
        assert_eq!(gap_times(1_000, 1_200, 100), [1_100]);
        assert_eq!(gap_times(1_000, 1_199, 100), [0_u64; 0]);
        assert_eq!(gap_times(1_000, 1_000, 100), [0_u64; 0]);
        assert_eq!(gap_times(2_000, 1_000, 100), [0_u64; 0]);
    }

    #[test]
    fn the_minimum_gap_follows_the_frame_rate() {
        assert_eq!(min_gap_ms(None), 100);
        assert_eq!(min_gap_ms(Some(30.0)), 100);
        assert_eq!(min_gap_ms(Some(4.0)), 250);
        assert_eq!(min_gap_ms(Some(0.0)), 100);
    }

    #[test]
    fn the_default_frame_is_extracted_first_then_ascending() {
        let mut timeline = timeline(HOUR);
        let order = drain(&mut timeline);
        assert_eq!(order[0], 180_000);
        assert!(order[1..].is_sorted());
    }

    #[test]
    fn the_chosen_frame_comes_first_then_the_default_then_ascending() {
        let mut timeline = FrameTimeline::<()>::default();
        timeline.start(
            Some(HOUR),
            None,
            Duration::from_secs(180),
            Some(Duration::from_millis(61_234)),
        );
        let order = drain(&mut timeline);
        assert_eq!(&order[..2], [61_234, 180_000]);
        assert!(order[2..].is_sorted());
        assert_eq!(timeline.default_ms(), Some(180_000));
    }

    #[test]
    fn filled_gaps_jump_the_queue_earliest_first() {
        let mut timeline = timeline(HOUR);
        let first = timeline.next_job().expect("first job");
        let gap = timeline
            .gap_after(first)
            .expect("gap after the first frame");
        let next = timeline
            .iter()
            .map(|(ms, _)| ms)
            .find(|ms| *ms > first)
            .expect("next frame");
        assert_eq!(
            gap,
            Gap {
                from: first,
                to: next
            }
        );
        timeline.fill(gap);
        let filled = gap_times(first, next, 100);
        assert_eq!(filled.len(), FILL_FRAMES as usize);
        assert_eq!(drain(&mut timeline)[..filled.len()], filled);
    }

    #[test]
    fn a_gap_can_be_filled_again_until_it_is_too_small() {
        let mut timeline = timeline(100.0);
        let mut from = timeline.iter().next().expect("first frame").0;
        let mut fills = 0;
        while let Some(gap) = timeline.gap_after(from) {
            timeline.fill(gap);
            from = timeline
                .iter()
                .map(|(ms, _)| ms)
                .find(|ms| *ms > from)
                .expect("frame after the fill");
            fills += 1;
            assert!(fills < MAX_FRAMES, "must stop");
        }
        let times: Vec<_> = timeline.iter().map(|(ms, _)| ms).collect();
        assert!(times.windows(2).all(|pair| pair[1] - pair[0] >= 100));
    }

    #[test]
    fn the_last_gap_ends_before_the_end_of_the_video() {
        let timeline = timeline(100.0);
        let last = timeline.iter().last().expect("frames").0;
        let gap = timeline.gap_after(last).expect("gap to the end");
        assert_eq!(gap.to, 99_900);
        assert_eq!(timeline.duration(), Some(Duration::from_secs(100)));
    }

    #[test]
    fn nothing_can_be_filled_without_a_known_length_after_the_last_frame() {
        let mut timeline = FrameTimeline::<()>::default();
        timeline.start(None, None, Duration::ZERO, None);
        assert_eq!(drain(&mut timeline), [0]);
        assert_eq!(timeline.gap_after(0), None);
        assert_eq!(timeline.gap_before(0), None);
        assert_eq!(timeline.duration(), None);
    }

    #[test]
    fn gap_before_reaches_back_to_the_start() {
        let timeline = timeline(HOUR);
        let first = timeline.iter().next().expect("first frame").0;
        assert_eq!(timeline.gap_before(first), Some(Gap { from: 0, to: first }));
        let second = timeline.iter().nth(1).expect("second frame").0;
        assert_eq!(
            timeline.gap_before(second),
            Some(Gap {
                from: first,
                to: second
            })
        );
    }

    #[test]
    fn the_frame_limit_stops_filling() {
        let mut timeline = timeline(HOUR);
        loop {
            let Some(gap) = timeline.iter().find_map(|(ms, _)| timeline.gap_after(ms)) else {
                break;
            };
            timeline.fill(gap);
        }
        assert_eq!(timeline.iter().count(), MAX_FRAMES);
        assert!(timeline.at_limit());
        assert_eq!(timeline.gap_after(0), None);
    }

    #[test]
    fn the_byte_limit_stops_filling() {
        let mut timeline = timeline(HOUR);
        let first = timeline.next_job().expect("first job");
        assert!(timeline.gap_after(first).is_some());
        timeline.finish(first, (), MAX_BYTES);
        assert!(timeline.at_limit());
        assert_eq!(timeline.gap_after(first), None);
    }

    #[test]
    fn a_failed_frame_is_removed_and_counted() {
        let mut timeline = timeline(HOUR);
        let total = timeline.progress().1;
        let first = timeline.next_job().expect("first job");
        timeline.fail(first);
        assert_eq!(timeline.failed(), 1);
        assert_eq!(timeline.progress(), (0, total - 1));
        assert!(timeline.iter().all(|(ms, _)| ms != first));
    }

    #[test]
    fn neighbors_skip_frames_that_are_not_ready() {
        let mut timeline = timeline(HOUR);
        let order = drain(&mut timeline);
        let mut sorted = order;
        sorted.sort_unstable();
        timeline.finish(sorted[0], (), 1);
        timeline.finish(sorted[3], (), 1);
        assert_eq!(timeline.ready_neighbor(sorted[0], true), Some(sorted[3]));
        assert_eq!(timeline.ready_neighbor(sorted[3], false), Some(sorted[0]));
        assert_eq!(timeline.ready_neighbor(sorted[3], true), None);
        assert_eq!(timeline.ready_neighbor(sorted[0], false), None);
        assert_eq!(timeline.frame(sorted[0]), Some(&()));
        assert_eq!(timeline.frame(sorted[1]), None);
    }
}
