//! Browser-compatible scheduling for incremental frontier diffusion.
//!
//! This module intentionally owns only queue state and scheduling. Territory
//! map reads and writes stay with the caller supplied to [`FrontierDiffusion::process`].

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use thiserror::Error;

pub const REGULAR_QUEUE_LIMIT: usize = 16_384;
pub const PRIORITY_QUEUE_LIMIT: usize = 8_192;
pub const QUEUE_COMPACTION_THRESHOLD: usize = 4_096;

const QUEUED_REGULAR: u8 = 1;
const QUEUED_PRIORITY: u8 = 2;

/// Portable pending queue state. Consumed prefixes are deliberately omitted,
/// while duplicate and stale pending entries retain their exact order.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InfluenceRuntimeState {
    pub regular_queue: Vec<usize>,
    pub priority_queue: Vec<usize>,
    /// Sorted `(cell, queue_kind)` pairs for non-zero dense queue state.
    pub queued_cells: Vec<(usize, u8)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrontierCellResult {
    pub remains_frontier: bool,
    /// Queue the processed cell and its orthogonal neighbors as priority work.
    /// This models the browser controller-change listener and happens before
    /// the normal regular frontier requeue.
    pub priority_neighborhood: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiffusionQueueResult {
    pub processed_items: usize,
    pub stale_entries: usize,
    pub requeued_cells: usize,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum DiffusionError {
    #[error("cell {cell} is outside a diffusion grid containing {cell_count} cells")]
    InvalidCell { cell: usize, cell_count: usize },
    #[error("diffusion dimensions {width}x{height} do not match the configured {cell_count} cells")]
    InvalidDimensions {
        width: usize,
        height: usize,
        cell_count: usize,
    },
    #[error("{queue} queue contains {actual} pending entries, limit is {limit}")]
    QueueLimit {
        queue: &'static str,
        actual: usize,
        limit: usize,
    },
    #[error("queued cell state entries must be strictly sorted by unique cell index")]
    UnsortedQueuedCells,
    #[error("queued cell {cell} has invalid state {state}; expected 1 or 2")]
    InvalidQueuedState { cell: usize, state: u8 },
    #[error("queued cell {cell} with state {state} has no matching pending queue entry")]
    MissingQueueEntry { cell: usize, state: u8 },
}

/// Ordered two-lane frontier work queue with browser-equivalent upgrade and
/// stale-entry behavior.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrontierDiffusion {
    cell_count: usize,
    regular_queue: Vec<usize>,
    regular_cursor: usize,
    priority_queue: Vec<usize>,
    priority_cursor: usize,
    queued: Vec<u8>,
}

impl FrontierDiffusion {
    pub fn empty(cell_count: usize) -> Self {
        Self {
            cell_count,
            regular_queue: Vec::new(),
            regular_cursor: 0,
            priority_queue: Vec::new(),
            priority_cursor: 0,
            queued: vec![0; cell_count],
        }
    }

    pub fn from_runtime_state(
        cell_count: usize,
        state: InfluenceRuntimeState,
    ) -> Result<Self, DiffusionError> {
        if state.regular_queue.len() > REGULAR_QUEUE_LIMIT {
            return Err(DiffusionError::QueueLimit {
                queue: "regular",
                actual: state.regular_queue.len(),
                limit: REGULAR_QUEUE_LIMIT,
            });
        }
        if state.priority_queue.len() > PRIORITY_QUEUE_LIMIT {
            return Err(DiffusionError::QueueLimit {
                queue: "priority",
                actual: state.priority_queue.len(),
                limit: PRIORITY_QUEUE_LIMIT,
            });
        }

        for &cell in state
            .regular_queue
            .iter()
            .chain(state.priority_queue.iter())
        {
            if cell >= cell_count {
                return Err(DiffusionError::InvalidCell { cell, cell_count });
            }
        }

        let regular_cells: HashSet<_> = state.regular_queue.iter().copied().collect();
        let priority_cells: HashSet<_> = state.priority_queue.iter().copied().collect();
        let mut queued = vec![0; cell_count];
        let mut previous_cell = None;

        for &(cell, queue_state) in &state.queued_cells {
            if cell >= cell_count {
                return Err(DiffusionError::InvalidCell { cell, cell_count });
            }
            if previous_cell.is_some_and(|previous| cell <= previous) {
                return Err(DiffusionError::UnsortedQueuedCells);
            }
            previous_cell = Some(cell);

            let present = match queue_state {
                QUEUED_REGULAR => regular_cells.contains(&cell),
                QUEUED_PRIORITY => priority_cells.contains(&cell),
                state => {
                    return Err(DiffusionError::InvalidQueuedState { cell, state });
                }
            };
            if !present {
                return Err(DiffusionError::MissingQueueEntry {
                    cell,
                    state: queue_state,
                });
            }
            queued[cell] = queue_state;
        }

        Ok(Self {
            cell_count,
            regular_queue: state.regular_queue,
            regular_cursor: 0,
            priority_queue: state.priority_queue,
            priority_cursor: 0,
            queued,
        })
    }

    /// Return a canonical checkpoint: pending suffixes only, but with all
    /// duplicates and stale entries preserved in order.
    pub fn runtime_state(&self) -> InfluenceRuntimeState {
        let queued_cells = self
            .queued
            .iter()
            .copied()
            .enumerate()
            .filter_map(|(cell, state)| (state != 0).then_some((cell, state)))
            .collect();

        InfluenceRuntimeState {
            regular_queue: self.regular_queue[self.regular_cursor..].to_vec(),
            priority_queue: self.priority_queue[self.priority_cursor..].to_vec(),
            queued_cells,
        }
    }

    /// Enqueue one cell. Returns whether a new queue entry was appended.
    pub fn enqueue_index(&mut self, index: usize, priority: bool) -> Result<bool, DiffusionError> {
        self.validate_cell(index)?;

        if priority {
            if self.queued[index] == QUEUED_PRIORITY {
                return Ok(false);
            }
            if self.priority_queue.len() - self.priority_cursor >= PRIORITY_QUEUE_LIMIT {
                return Ok(false);
            }
            self.queued[index] = QUEUED_PRIORITY;
            self.priority_queue.push(index);
            return Ok(true);
        }

        if self.queued[index] != 0 {
            return Ok(false);
        }
        if self.regular_queue.len() - self.regular_cursor >= REGULAR_QUEUE_LIMIT {
            return Ok(false);
        }
        self.queued[index] = QUEUED_REGULAR;
        self.regular_queue.push(index);
        Ok(true)
    }

    /// Enqueue center, left, right, up, and down, in that exact order.
    pub fn enqueue_cell(
        &mut self,
        index: usize,
        width: usize,
        height: usize,
        priority: bool,
    ) -> Result<usize, DiffusionError> {
        self.validate_dimensions(width, height)?;
        self.validate_cell(index)?;
        Ok(self.enqueue_cell_validated(index, width, height, priority))
    }

    /// Process the priority snapshot before the regular snapshot. Entries
    /// appended by callbacks are deferred unless they make an older stale slot
    /// valid again (the browser queue's intentional ABA behavior).
    pub fn process<F>(
        &mut self,
        width: usize,
        height: usize,
        budget: usize,
        mut process_cell: F,
    ) -> Result<DiffusionQueueResult, DiffusionError>
    where
        F: FnMut(usize) -> FrontierCellResult,
    {
        self.validate_dimensions(width, height)?;

        let priority_end = self.priority_queue.len();
        let regular_end = self.regular_queue.len();
        let mut result = DiffusionQueueResult::default();

        while self.priority_cursor < priority_end && result.processed_items < budget {
            let index = self.priority_queue[self.priority_cursor];
            self.priority_cursor += 1;
            if self.queued[index] != QUEUED_PRIORITY {
                result.stale_entries += 1;
                continue;
            }
            self.queued[index] = 0;
            result.processed_items += 1;
            self.apply_cell_result(index, width, height, process_cell(index), &mut result);
        }

        while self.regular_cursor < regular_end && result.processed_items < budget {
            let index = self.regular_queue[self.regular_cursor];
            self.regular_cursor += 1;
            if self.queued[index] != QUEUED_REGULAR {
                result.stale_entries += 1;
                continue;
            }
            self.queued[index] = 0;
            result.processed_items += 1;
            self.apply_cell_result(index, width, height, process_cell(index), &mut result);
        }

        self.compact_consumed_prefixes();
        Ok(result)
    }

    fn apply_cell_result(
        &mut self,
        index: usize,
        width: usize,
        height: usize,
        cell_result: FrontierCellResult,
        queue_result: &mut DiffusionQueueResult,
    ) {
        if cell_result.priority_neighborhood {
            queue_result.requeued_cells += self.enqueue_cell_validated(index, width, height, true);
        }
        if cell_result.remains_frontier
            && self
                .enqueue_index(index, false)
                .expect("processed diffusion cell remains in bounds")
        {
            queue_result.requeued_cells += 1;
        }
    }

    fn enqueue_cell_validated(
        &mut self,
        index: usize,
        width: usize,
        height: usize,
        priority: bool,
    ) -> usize {
        let x = index % width;
        let y = index / width;
        let mut appended = usize::from(
            self.enqueue_index(index, priority)
                .expect("validated center is in bounds"),
        );
        if x > 0 {
            appended += usize::from(
                self.enqueue_index(index - 1, priority)
                    .expect("validated left neighbor is in bounds"),
            );
        }
        if x + 1 < width {
            appended += usize::from(
                self.enqueue_index(index + 1, priority)
                    .expect("validated right neighbor is in bounds"),
            );
        }
        if y > 0 {
            appended += usize::from(
                self.enqueue_index(index - width, priority)
                    .expect("validated upper neighbor is in bounds"),
            );
        }
        if y + 1 < height {
            appended += usize::from(
                self.enqueue_index(index + width, priority)
                    .expect("validated lower neighbor is in bounds"),
            );
        }
        appended
    }

    fn validate_cell(&self, cell: usize) -> Result<(), DiffusionError> {
        if cell >= self.cell_count {
            return Err(DiffusionError::InvalidCell {
                cell,
                cell_count: self.cell_count,
            });
        }
        Ok(())
    }

    fn validate_dimensions(&self, width: usize, height: usize) -> Result<(), DiffusionError> {
        if width == 0 || height == 0 || width.checked_mul(height) != Some(self.cell_count) {
            return Err(DiffusionError::InvalidDimensions {
                width,
                height,
                cell_count: self.cell_count,
            });
        }
        Ok(())
    }

    fn compact_consumed_prefixes(&mut self) {
        if self.priority_cursor > QUEUE_COMPACTION_THRESHOLD {
            self.priority_queue.drain(..self.priority_cursor);
            self.priority_cursor = 0;
        }
        if self.regular_cursor > QUEUE_COMPACTION_THRESHOLD {
            self.regular_queue.drain(..self.regular_cursor);
            self.regular_cursor = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settled() -> FrontierCellResult {
        FrontierCellResult {
            remains_frontier: false,
            priority_neighborhood: false,
        }
    }

    fn frontier() -> FrontierCellResult {
        FrontierCellResult {
            remains_frontier: true,
            priority_neighborhood: false,
        }
    }

    #[test]
    fn enqueue_cell_uses_browser_neighbor_order() {
        let mut queue = FrontierDiffusion::empty(25);
        assert_eq!(queue.enqueue_cell(12, 5, 5, false), Ok(5));
        assert_eq!(queue.runtime_state().regular_queue, vec![12, 11, 13, 7, 17]);
        assert_eq!(
            queue.runtime_state().queued_cells,
            vec![(7, 1), (11, 1), (12, 1), (13, 1), (17, 1)]
        );
    }

    #[test]
    fn priority_is_processed_before_regular_work() {
        let mut queue = FrontierDiffusion::empty(9);
        queue.enqueue_index(1, false).unwrap();
        queue.enqueue_index(2, true).unwrap();
        let mut visited = Vec::new();

        let result = queue
            .process(3, 3, 1, |cell| {
                visited.push(cell);
                settled()
            })
            .unwrap();

        assert_eq!(visited, vec![2]);
        assert_eq!(result.processed_items, 1);
        assert_eq!(queue.runtime_state().regular_queue, vec![1]);
    }

    #[test]
    fn newly_appended_work_is_deferred_to_the_next_snapshot() {
        let mut queue = FrontierDiffusion::empty(9);
        queue.enqueue_index(4, false).unwrap();

        let first = queue.process(3, 3, 10, |_| frontier()).unwrap();
        assert_eq!(first.processed_items, 1);
        assert_eq!(first.requeued_cells, 1);
        assert_eq!(queue.runtime_state().regular_queue, vec![4]);

        let second = queue.process(3, 3, 10, |_| settled()).unwrap();
        assert_eq!(second.processed_items, 1);
        assert!(queue.runtime_state().regular_queue.is_empty());
    }

    #[test]
    fn priority_upgrade_preserves_exact_aba_behavior() {
        let mut queue = FrontierDiffusion::empty(25);
        queue.enqueue_index(12, false).unwrap();
        queue.enqueue_index(12, true).unwrap();

        let result = queue.process(5, 5, 10, |_| frontier()).unwrap();

        assert_eq!(result.processed_items, 2);
        assert_eq!(result.stale_entries, 0);
        assert_eq!(result.requeued_cells, 2);
        assert_eq!(queue.regular_queue, vec![12, 12, 12]);
        assert_eq!(queue.regular_cursor, 1);
        assert_eq!(queue.runtime_state().regular_queue, vec![12, 12]);
        assert_eq!(queue.runtime_state().queued_cells, vec![(12, 1)]);
    }

    #[test]
    fn controller_change_priority_neighborhood_precedes_regular_requeue() {
        let mut queue = FrontierDiffusion::empty(25);
        queue.enqueue_index(12, false).unwrap();

        let result = queue
            .process(5, 5, 10, |_| FrontierCellResult {
                remains_frontier: true,
                priority_neighborhood: true,
            })
            .unwrap();

        assert_eq!(result.processed_items, 1);
        assert_eq!(result.requeued_cells, 5);
        let state = queue.runtime_state();
        assert!(state.regular_queue.is_empty());
        assert_eq!(state.priority_queue, vec![12, 11, 13, 7, 17]);
        assert_eq!(
            state.queued_cells,
            vec![(7, 2), (11, 2), (12, 2), (13, 2), (17, 2)]
        );
    }

    #[test]
    fn stale_entries_do_not_consume_budget() {
        let state = InfluenceRuntimeState {
            regular_queue: vec![3, 4],
            priority_queue: vec![3],
            queued_cells: vec![(3, 2), (4, 1)],
        };
        let mut queue = FrontierDiffusion::from_runtime_state(9, state).unwrap();
        let mut visited = Vec::new();

        let result = queue
            .process(3, 3, 2, |cell| {
                visited.push(cell);
                settled()
            })
            .unwrap();

        assert_eq!(visited, vec![3, 4]);
        assert_eq!(result.processed_items, 2);
        assert_eq!(result.stale_entries, 1);
    }

    #[test]
    fn enqueue_caps_are_exact_and_non_destructive() {
        let mut regular = FrontierDiffusion::empty(REGULAR_QUEUE_LIMIT + 1);
        for cell in 0..REGULAR_QUEUE_LIMIT {
            assert_eq!(regular.enqueue_index(cell, false), Ok(true));
        }
        assert_eq!(regular.enqueue_index(REGULAR_QUEUE_LIMIT, false), Ok(false));
        assert_eq!(regular.queued[REGULAR_QUEUE_LIMIT], 0);

        let mut priority = FrontierDiffusion::empty(PRIORITY_QUEUE_LIMIT + 1);
        for cell in 0..PRIORITY_QUEUE_LIMIT {
            assert_eq!(priority.enqueue_index(cell, true), Ok(true));
        }
        assert_eq!(
            priority.enqueue_index(PRIORITY_QUEUE_LIMIT, true),
            Ok(false)
        );
        assert_eq!(priority.queued[PRIORITY_QUEUE_LIMIT], 0);
    }

    #[test]
    fn consumed_prefix_compacts_only_after_strict_threshold() {
        let cell_count = QUEUE_COMPACTION_THRESHOLD + 1;
        let mut queue = FrontierDiffusion::empty(cell_count);
        for cell in 0..QUEUE_COMPACTION_THRESHOLD {
            queue.enqueue_index(cell, true).unwrap();
        }

        queue
            .process(cell_count, 1, QUEUE_COMPACTION_THRESHOLD, |_| settled())
            .unwrap();
        assert_eq!(queue.priority_cursor, QUEUE_COMPACTION_THRESHOLD);
        assert_eq!(queue.priority_queue.len(), QUEUE_COMPACTION_THRESHOLD);
        assert!(queue.runtime_state().priority_queue.is_empty());

        queue
            .enqueue_index(QUEUE_COMPACTION_THRESHOLD, true)
            .unwrap();
        queue.process(cell_count, 1, 1, |_| settled()).unwrap();
        assert_eq!(queue.priority_cursor, 0);
        assert!(queue.priority_queue.is_empty());
    }

    #[test]
    fn restore_rejects_oversize_queues() {
        let state = InfluenceRuntimeState {
            regular_queue: vec![0; REGULAR_QUEUE_LIMIT + 1],
            priority_queue: Vec::new(),
            queued_cells: vec![(0, 1)],
        };
        assert_eq!(
            FrontierDiffusion::from_runtime_state(1, state),
            Err(DiffusionError::QueueLimit {
                queue: "regular",
                actual: REGULAR_QUEUE_LIMIT + 1,
                limit: REGULAR_QUEUE_LIMIT,
            })
        );
    }

    #[test]
    fn restore_rejects_invalid_sparse_state() {
        let missing = InfluenceRuntimeState {
            regular_queue: vec![1],
            priority_queue: Vec::new(),
            queued_cells: vec![(2, 1)],
        };
        assert_eq!(
            FrontierDiffusion::from_runtime_state(4, missing),
            Err(DiffusionError::MissingQueueEntry { cell: 2, state: 1 })
        );

        let unsorted = InfluenceRuntimeState {
            regular_queue: vec![1, 2],
            priority_queue: Vec::new(),
            queued_cells: vec![(2, 1), (1, 1)],
        };
        assert_eq!(
            FrontierDiffusion::from_runtime_state(4, unsorted),
            Err(DiffusionError::UnsortedQueuedCells)
        );

        let invalid_kind = InfluenceRuntimeState {
            regular_queue: vec![1],
            priority_queue: Vec::new(),
            queued_cells: vec![(1, 3)],
        };
        assert_eq!(
            FrontierDiffusion::from_runtime_state(4, invalid_kind),
            Err(DiffusionError::InvalidQueuedState { cell: 1, state: 3 })
        );
    }

    #[test]
    fn invalid_dimensions_do_not_mutate_the_queue() {
        let mut queue = FrontierDiffusion::empty(9);
        queue.enqueue_index(4, false).unwrap();
        let before = queue.clone();
        let mut callback_called = false;

        assert_eq!(
            queue.process(2, 4, 1, |_| {
                callback_called = true;
                settled()
            }),
            Err(DiffusionError::InvalidDimensions {
                width: 2,
                height: 4,
                cell_count: 9,
            })
        );
        assert!(!callback_called);
        assert_eq!(queue, before);
    }
}
