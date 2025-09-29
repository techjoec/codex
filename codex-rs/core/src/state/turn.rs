//! Turn-scoped state and active turn metadata scaffolding.

use indexmap::IndexMap;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::AbortHandle;

use codex_protocol::models::ResponseInputItem;
use tokio::sync::oneshot;

use crate::protocol::ReviewDecision;
use crate::tasks::SessionTask;

/// Default per-turn budget for tool output (24 KiB).
pub(crate) const PER_TURN_OUTPUT_MAX_BYTES: usize = 24 * 1024;

/// Maximum bytes reserved for the per-turn truncation notice.
const TURN_OUTPUT_NOTICE_RESERVE_BYTES: usize = 128;

/// Truncation message appended when the per-turn output budget is exceeded.
pub(crate) const TURN_OUTPUT_TRUNCATION_NOTICE: &str =
    "[turn output truncated after reaching 24 KiB; refine your request or use /relax]";

/// Metadata about the currently running turn.
pub(crate) struct ActiveTurn {
    pub(crate) tasks: IndexMap<String, RunningTask>,
    pub(crate) turn_state: Arc<Mutex<TurnState>>,
}

impl Default for ActiveTurn {
    fn default() -> Self {
        Self {
            tasks: IndexMap::new(),
            turn_state: Arc::new(Mutex::new(TurnState::default())),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TaskKind {
    Regular,
    Review,
    Compact,
}

#[derive(Clone)]
pub(crate) struct RunningTask {
    pub(crate) handle: AbortHandle,
    pub(crate) kind: TaskKind,
    pub(crate) task: Arc<dyn SessionTask>,
}

impl ActiveTurn {
    pub(crate) fn add_task(&mut self, sub_id: String, task: RunningTask) {
        self.tasks.insert(sub_id, task);
    }

    pub(crate) fn remove_task(&mut self, sub_id: &str) -> bool {
        self.tasks.swap_remove(sub_id);
        self.tasks.is_empty()
    }

    pub(crate) fn drain_tasks(&mut self) -> IndexMap<String, RunningTask> {
        std::mem::take(&mut self.tasks)
    }
}

/// Mutable state for a single turn.
pub(crate) struct TurnState {
    pending_approvals: HashMap<String, oneshot::Sender<ReviewDecision>>,
    pending_input: Vec<ResponseInputItem>,
    tool_output_budget: ToolOutputBudget,
    metrics: TurnMetrics,
}

impl TurnState {
    pub(crate) fn reserve_tool_output(
        &mut self,
        desired_bytes: usize,
        notice_len: usize,
    ) -> ToolBudgetDecision {
        self.tool_output_budget
            .reserve(desired_bytes, notice_len, &mut self.metrics)
    }

    pub(crate) fn record_command_blocked(&mut self) {
        self.metrics.commands_blocked = self.metrics.commands_blocked.saturating_add(1);
    }

    pub(crate) fn record_log_tail(&mut self) {
        self.metrics.log_tail_invocations = self.metrics.log_tail_invocations.saturating_add(1);
    }

    pub(crate) fn drain_metrics(&mut self) -> TurnMetrics {
        std::mem::take(&mut self.metrics)
    }

    pub(crate) fn insert_pending_approval(
        &mut self,
        key: String,
        tx: oneshot::Sender<ReviewDecision>,
    ) -> Option<oneshot::Sender<ReviewDecision>> {
        self.pending_approvals.insert(key, tx)
    }

    pub(crate) fn remove_pending_approval(
        &mut self,
        key: &str,
    ) -> Option<oneshot::Sender<ReviewDecision>> {
        self.pending_approvals.remove(key)
    }

    pub(crate) fn clear_pending(&mut self) {
        self.pending_approvals.clear();
        self.pending_input.clear();
    }

    pub(crate) fn push_pending_input(&mut self, input: ResponseInputItem) {
        self.pending_input.push(input);
    }

    pub(crate) fn take_pending_input(&mut self) -> Vec<ResponseInputItem> {
        if self.pending_input.is_empty() {
            Vec::with_capacity(0)
        } else {
            let mut ret = Vec::new();
            std::mem::swap(&mut ret, &mut self.pending_input);
            ret
        }
    }
}

impl ActiveTurn {
    /// Clear any pending approvals and input buffered for the current turn.
    pub(crate) async fn clear_pending(&self) {
        let mut ts = self.turn_state.lock().await;
        ts.clear_pending();
    }

    /// Best-effort, non-blocking variant for synchronous contexts (Drop/interrupt).
    pub(crate) fn try_clear_pending_sync(&self) {
        if let Ok(mut ts) = self.turn_state.try_lock() {
            ts.clear_pending();
        }
    }
}

impl Default for TurnState {
    fn default() -> Self {
        Self {
            pending_approvals: HashMap::new(),
            pending_input: Vec::new(),
            tool_output_budget: ToolOutputBudget::new(PER_TURN_OUTPUT_MAX_BYTES),
            metrics: TurnMetrics::default(),
        }
    }
}

#[derive(Debug, Default, Clone)]
pub(crate) struct TurnMetrics {
    pub(crate) bytes_served: usize,
    pub(crate) bytes_trimmed: usize,
    pub(crate) outputs_truncated: usize,
    pub(crate) commands_blocked: usize,
    pub(crate) log_tail_invocations: usize,
}

impl TurnMetrics {
    pub(crate) fn is_empty(&self) -> bool {
        self.bytes_served == 0
            && self.bytes_trimmed == 0
            && self.outputs_truncated == 0
            && self.commands_blocked == 0
            && self.log_tail_invocations == 0
    }
}

#[derive(Debug)]
struct ToolOutputBudget {
    max_bytes: usize,
    used_bytes: usize,
}

impl ToolOutputBudget {
    const fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            used_bytes: 0,
        }
    }

    fn remaining(&self) -> usize {
        self.max_bytes.saturating_sub(self.used_bytes)
    }

    fn consume(&mut self, bytes: usize) {
        let new_total = self.used_bytes.saturating_add(bytes);
        self.used_bytes = new_total.min(self.max_bytes);
    }

    fn reserve(
        &mut self,
        desired_bytes: usize,
        notice_len: usize,
        metrics: &mut TurnMetrics,
    ) -> ToolBudgetDecision {
        if desired_bytes == 0 {
            return ToolBudgetDecision {
                allowed_content_bytes: 0,
                notice_bytes: 0,
                truncated: false,
            };
        }

        let remaining = self.remaining();

        if desired_bytes <= remaining {
            self.consume(desired_bytes);
            metrics.bytes_served = metrics.bytes_served.saturating_add(desired_bytes);
            return ToolBudgetDecision {
                allowed_content_bytes: desired_bytes,
                notice_bytes: 0,
                truncated: false,
            };
        }

        let notice_cap = TURN_OUTPUT_NOTICE_RESERVE_BYTES.min(notice_len);
        let (allowed_content_bytes, notice_bytes) = if remaining == 0 {
            (0, notice_cap)
        } else {
            let notice_bytes = remaining.min(notice_cap);
            let content_bytes = remaining.saturating_sub(notice_bytes);
            (content_bytes, notice_bytes)
        };

        let served_bytes = allowed_content_bytes.saturating_add(notice_bytes);
        self.consume(served_bytes);

        metrics.bytes_served = metrics.bytes_served.saturating_add(served_bytes);
        metrics.bytes_trimmed = metrics
            .bytes_trimmed
            .saturating_add(desired_bytes.saturating_sub(allowed_content_bytes));
        metrics.outputs_truncated = metrics.outputs_truncated.saturating_add(1);

        ToolBudgetDecision {
            allowed_content_bytes,
            notice_bytes,
            truncated: true,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ToolBudgetDecision {
    pub(crate) allowed_content_bytes: usize,
    pub(crate) notice_bytes: usize,
    pub(crate) truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserves_full_output_when_under_budget() {
        let mut state = TurnState::default();
        let decision = state.reserve_tool_output(1024, TURN_OUTPUT_TRUNCATION_NOTICE.len());

        assert!(!decision.truncated);
        assert_eq!(decision.allowed_content_bytes, 1024);
        assert_eq!(state.metrics.bytes_served, 1024);
        assert_eq!(state.metrics.bytes_trimmed, 0);
        assert_eq!(state.metrics.outputs_truncated, 0);
    }

    #[test]
    fn reserves_with_truncation_and_notice() {
        let mut state = TurnState::default();
        let _ = state.reserve_tool_output(PER_TURN_OUTPUT_MAX_BYTES - 100, 0);
        state.drain_metrics();

        let decision = state.reserve_tool_output(200, 80);
        assert!(decision.truncated);
        assert_eq!(decision.allowed_content_bytes, 20);
        assert_eq!(decision.notice_bytes, 80);
        assert_eq!(state.metrics.bytes_served, 100);
        assert_eq!(state.metrics.bytes_trimmed, 180);
        assert_eq!(state.metrics.outputs_truncated, 1);
    }

    #[test]
    fn reserves_notice_even_when_budget_exhausted() {
        let mut state = TurnState::default();
        let _ = state.reserve_tool_output(PER_TURN_OUTPUT_MAX_BYTES, 0);
        state.drain_metrics();

        let decision = state.reserve_tool_output(512, 64);
        assert!(decision.truncated);
        assert_eq!(decision.allowed_content_bytes, 0);
        assert_eq!(decision.notice_bytes, 64);
        assert_eq!(state.metrics.bytes_served, 64);
        assert_eq!(state.metrics.bytes_trimmed, 512);
        assert_eq!(state.metrics.outputs_truncated, 1);
    }

    #[test]
    fn draining_metrics_resets_counters() {
        let mut state = TurnState::default();
        let _ = state.reserve_tool_output(128, 0);
        let metrics = state.drain_metrics();
        assert_eq!(metrics.bytes_served, 128);
        assert!(state.metrics.is_empty());
    }

    #[test]
    fn recording_log_tail_increments_metric() {
        let mut state = TurnState::default();
        state.record_log_tail();
        assert_eq!(state.metrics.log_tail_invocations, 1);
    }
}
