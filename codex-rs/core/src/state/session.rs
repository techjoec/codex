//! Session-wide mutable state.

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::hash_map::Entry;
use std::time::Duration;
use std::time::Instant;

use codex_protocol::models::ResponseItem;

use crate::conversation_history::ConversationHistory;
use crate::protocol::RateLimitSnapshot;
use crate::protocol::TokenUsage;
use crate::protocol::TokenUsageInfo;
use crate::truncate::truncate_middle;

const DEFAULT_REPEAT_COMMAND_REPEATS: usize = 3;
const DEFAULT_REPEAT_COMMAND_WINDOW_SECS: u64 = 120;
const REPEAT_COMMAND_OUTPUT_PREVIEW_BYTES: usize = 256;

/// Persistent, session-scoped state previously stored directly on `Session`.
#[derive(Default)]
pub(crate) struct SessionState {
    pub(crate) approved_commands: HashSet<Vec<String>>,
    pub(crate) history: ConversationHistory,
    pub(crate) token_info: Option<TokenUsageInfo>,
    pub(crate) latest_rate_limits: Option<RateLimitSnapshot>,
    repeat_command_breaker: RepeatCommandBreaker,
    code_read_index: HashMap<String, IntervalSet>,
}

impl SessionState {
    /// Create a new session state mirroring previous `State::default()` semantics.
    pub(crate) fn new() -> Self {
        Self {
            history: ConversationHistory::new(),
            repeat_command_breaker: RepeatCommandBreaker::default(),
            ..Default::default()
        }
    }

    // History helpers
    pub(crate) fn record_items<I>(&mut self, items: I)
    where
        I: IntoIterator,
        I::Item: std::ops::Deref<Target = ResponseItem>,
    {
        self.history.record_items(items)
    }

    pub(crate) fn history_snapshot(&self) -> Vec<ResponseItem> {
        self.history.contents()
    }

    pub(crate) fn replace_history(&mut self, items: Vec<ResponseItem>) {
        self.history.replace(items);
    }

    // Approved command helpers
    pub(crate) fn add_approved_command(&mut self, cmd: Vec<String>) {
        self.approved_commands.insert(cmd);
    }

    pub(crate) fn approved_commands_ref(&self) -> &HashSet<Vec<String>> {
        &self.approved_commands
    }

    // Token/rate limit helpers
    pub(crate) fn update_token_info_from_usage(
        &mut self,
        usage: &TokenUsage,
        model_context_window: Option<u64>,
    ) {
        self.token_info = TokenUsageInfo::new_or_append(
            &self.token_info,
            &Some(usage.clone()),
            model_context_window,
        );
    }

    pub(crate) fn set_rate_limits(&mut self, snapshot: RateLimitSnapshot) {
        self.latest_rate_limits = Some(snapshot);
    }

    pub(crate) fn token_info_and_rate_limits(
        &self,
    ) -> (Option<TokenUsageInfo>, Option<RateLimitSnapshot>) {
        (self.token_info.clone(), self.latest_rate_limits.clone())
    }

    pub(crate) fn check_repeat_command(
        &mut self,
        command: &[String],
        now: Instant,
    ) -> Option<RepeatCommandBlock> {
        self.repeat_command_breaker.check(command, now)
    }

    pub(crate) fn record_repeat_command(&mut self, command: &[String], output: &str, now: Instant) {
        self.repeat_command_breaker.record(command, output, now);
    }

    // Pending input/approval moved to TurnState.

    pub(crate) fn compute_unserved_code_ranges(
        &self,
        path: &str,
        ranges: &[(usize, usize)],
    ) -> (Vec<(usize, usize)>, bool) {
        let Some(intervals) = self.code_read_index.get(path) else {
            return (ranges.to_vec(), false);
        };

        let mut uncovered = Vec::new();
        let mut had_overlap = false;

        for &(start, end) in ranges {
            if start == 0 || end == 0 || start > end {
                continue;
            }
            let missing = intervals.subtract(start, end);
            if !missing.is_empty() {
                uncovered.extend(missing.iter().copied());
            }
            let requested_len = end.saturating_sub(start).saturating_add(1);
            let uncovered_len = missing
                .iter()
                .map(|(s, e)| e.saturating_sub(*s).saturating_add(1))
                .sum::<usize>();
            if uncovered_len < requested_len {
                had_overlap = true;
            }
        }

        if uncovered.is_empty() {
            (Vec::new(), had_overlap)
        } else {
            (uncovered, had_overlap)
        }
    }

    pub(crate) fn record_served_code_ranges(&mut self, path: &str, ranges: &[(usize, usize)]) {
        if ranges.is_empty() {
            return;
        }
        let entry = self
            .code_read_index
            .entry(path.to_string())
            .or_insert_with(IntervalSet::default);

        for &(start, end) in ranges {
            entry.insert(start, end);
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RepeatCommandBlock {
    pub(crate) repeat_count: usize,
    pub(crate) window: Duration,
    pub(crate) last_excerpt: Option<String>,
}

#[derive(Debug, Default)]
struct RepeatCommandBreaker {
    entries: HashMap<Vec<String>, RepeatCommandEntry>,
    config: RepeatCommandConfig,
}

#[derive(Debug, Default)]
struct IntervalSet {
    intervals: Vec<(usize, usize)>,
}

impl IntervalSet {
    fn subtract(&self, start: usize, end: usize) -> Vec<(usize, usize)> {
        if start == 0 || end == 0 || start > end {
            return Vec::new();
        }

        if self.intervals.is_empty() {
            return vec![(start, end)];
        }

        let mut uncovered = Vec::new();
        let mut cursor = start;

        for &(lo, hi) in &self.intervals {
            if hi < cursor {
                continue;
            }
            if lo > end {
                break;
            }
            if lo > cursor {
                let gap_end = (lo - 1).min(end);
                if cursor <= gap_end {
                    uncovered.push((cursor, gap_end));
                }
            }
            if hi >= cursor {
                cursor = hi.saturating_add(1);
                if cursor > end {
                    return uncovered;
                }
            }
        }

        if cursor <= end {
            uncovered.push((cursor, end));
        }

        uncovered
    }

    fn insert(&mut self, start: usize, end: usize) {
        if start == 0 || end == 0 || start > end {
            return;
        }

        let mut merged = Vec::with_capacity(self.intervals.len() + 1);
        let mut new_start = start;
        let mut new_end = end;
        let mut inserted = false;

        for &(lo, hi) in &self.intervals {
            if hi.saturating_add(1) < new_start {
                merged.push((lo, hi));
                continue;
            }

            if lo > new_end.saturating_add(1) {
                if !inserted {
                    merged.push((new_start, new_end));
                    inserted = true;
                }
                merged.push((lo, hi));
                continue;
            }

            new_start = new_start.min(lo);
            new_end = new_end.max(hi);
        }

        if !inserted {
            merged.push((new_start, new_end));
        }

        merged.sort_by_key(|(lo, _)| *lo);
        self.intervals = merged;
    }
}

#[derive(Debug, Clone, Copy)]
struct RepeatCommandConfig {
    max_repeats: usize,
    window: Duration,
}

impl Default for RepeatCommandConfig {
    fn default() -> Self {
        Self {
            max_repeats: DEFAULT_REPEAT_COMMAND_REPEATS,
            window: Duration::from_secs(DEFAULT_REPEAT_COMMAND_WINDOW_SECS),
        }
    }
}

#[derive(Debug)]
struct RepeatCommandEntry {
    last_fingerprint: u64,
    repeat_count: usize,
    last_seen: Instant,
    last_excerpt: Option<String>,
}

impl RepeatCommandBreaker {
    fn is_enabled(&self) -> bool {
        self.config.max_repeats > 1
    }

    fn check(&mut self, command: &[String], now: Instant) -> Option<RepeatCommandBlock> {
        if !self.is_enabled() || command.is_empty() {
            return None;
        }

        let Some(entry) = self.entries.get_mut(command) else {
            return None;
        };

        if now.saturating_duration_since(entry.last_seen) > self.config.window {
            self.entries.remove(command);
            return None;
        }

        let threshold = self.config.max_repeats.saturating_sub(1);
        if threshold == 0 {
            return None;
        }

        if entry.repeat_count >= threshold {
            Some(RepeatCommandBlock {
                repeat_count: entry.repeat_count,
                window: self.config.window,
                last_excerpt: entry.last_excerpt.clone(),
            })
        } else {
            None
        }
    }

    fn record(&mut self, command: &[String], output: &str, now: Instant) {
        if !self.is_enabled() || command.is_empty() {
            return;
        }

        let fingerprint = fingerprint_output(output);
        let excerpt = output_preview(output);

        match self.entries.entry(command.to_vec()) {
            Entry::Occupied(mut occ) => {
                let entry = occ.get_mut();
                if now.saturating_duration_since(entry.last_seen) > self.config.window
                    || entry.last_fingerprint != fingerprint
                {
                    entry.repeat_count = 1;
                    entry.last_fingerprint = fingerprint;
                } else {
                    entry.repeat_count = (entry.repeat_count + 1).min(self.config.max_repeats);
                }
                entry.last_seen = now;
                entry.last_excerpt = excerpt;
            }
            Entry::Vacant(vacant) => {
                vacant.insert(RepeatCommandEntry {
                    last_fingerprint: fingerprint,
                    repeat_count: 1,
                    last_seen: now,
                    last_excerpt: excerpt,
                });
            }
        }
    }
}

fn fingerprint_output(output: &str) -> u64 {
    use std::hash::Hash;
    use std::hash::Hasher;

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    output.hash(&mut hasher);
    hasher.finish()
}

fn output_preview(output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return None;
    }

    let (truncated, _) = truncate_middle(trimmed, REPEAT_COMMAND_OUTPUT_PREVIEW_BYTES);
    Some(truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(cmd: &[&str]) -> Vec<String> {
        cmd.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn interval_set_records_and_subtracts() {
        let mut set = IntervalSet::default();
        assert_eq!(set.subtract(5, 10), vec![(5, 10)]);
        set.insert(5, 10);
        assert!(set.subtract(5, 10).is_empty());
        assert_eq!(set.subtract(8, 15), vec![(11, 15)]);
    }

    #[test]
    fn session_state_tracks_code_ranges() {
        let mut state = SessionState::new();
        let (unserved, overlap) = state.compute_unserved_code_ranges("file.rs", &[(1, 5)]);
        assert_eq!(unserved, vec![(1, 5)]);
        assert!(!overlap);

        state.record_served_code_ranges("file.rs", &[(1, 3)]);
        let (unserved, overlap) = state.compute_unserved_code_ranges("file.rs", &[(1, 5)]);
        assert_eq!(unserved, vec![(4, 5)]);
        assert!(overlap);
    }

    #[test]
    fn breaker_blocks_after_repeated_identical_output() {
        let mut breaker = RepeatCommandBreaker::default();
        let cmd = command(&["ls", "-l"]);
        let now = Instant::now();

        assert!(breaker.check(&cmd, now).is_none());

        breaker.record(&cmd, "alpha", now);
        assert!(breaker.check(&cmd, now + Duration::from_secs(1)).is_none());

        breaker.record(&cmd, "alpha", now + Duration::from_secs(2));
        let block = breaker
            .check(&cmd, now + Duration::from_secs(3))
            .expect("should block third run");
        assert_eq!(block.repeat_count, 2);
        assert_eq!(
            block.window,
            Duration::from_secs(DEFAULT_REPEAT_COMMAND_WINDOW_SECS)
        );
        assert_eq!(block.last_excerpt.as_deref(), Some("alpha"));
    }

    #[test]
    fn breaker_resets_when_output_changes() {
        let mut breaker = RepeatCommandBreaker::default();
        let cmd = command(&["git", "status"]);
        let now = Instant::now();

        breaker.record(&cmd, "one", now);
        breaker.record(&cmd, "one", now + Duration::from_secs(1));
        assert!(breaker.check(&cmd, now + Duration::from_secs(2)).is_some());

        breaker.record(&cmd, "two", now + Duration::from_secs(3));
        assert!(breaker.check(&cmd, now + Duration::from_secs(4)).is_none());
    }

    #[test]
    fn breaker_expires_after_window() {
        let mut breaker = RepeatCommandBreaker::default();
        let cmd = command(&["rg", "foo"]);
        let now = Instant::now();

        breaker.record(&cmd, "same", now);
        breaker.record(&cmd, "same", now + Duration::from_secs(1));
        assert!(breaker.check(&cmd, now + Duration::from_secs(2)).is_some());

        assert!(
            breaker
                .check(
                    &cmd,
                    now + Duration::from_secs(DEFAULT_REPEAT_COMMAND_WINDOW_SECS + 5)
                )
                .is_none()
        );
    }
}
