use std::collections::{HashSet, VecDeque};

use crate::documents::NativeDocumentKey;

#[cfg(test)]
const MAX_TRACKED_HISTORY_DOCUMENTS: usize = 32;
#[cfg(test)]
const MAX_RETAINED_HISTORY_STEPS: usize = 256;

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExecutionId(u64);

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HistoryRevision(u64);

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HistoryDirection {
    Undo,
    Redo,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HistoryClaim {
    key: NativeDocumentKey,
    direction: HistoryDirection,
    expected_step: ExecutionId,
    revision: HistoryRevision,
}

pub(crate) struct HistoryLedger {
    documents: VecDeque<DocumentHistory>,
    #[cfg(test)]
    next_revision: u64,
    #[cfg(test)]
    revision_exhausted: bool,
}

struct DocumentHistory {
    key: NativeDocumentKey,
    #[cfg(test)]
    revision: HistoryRevision,
    #[cfg(test)]
    undo: VecDeque<ExecutionId>,
    #[cfg(test)]
    redo: VecDeque<ExecutionId>,
}

impl HistoryLedger {
    pub(crate) const fn new() -> Self {
        Self {
            documents: VecDeque::new(),
            #[cfg(test)]
            next_revision: 1,
            #[cfg(test)]
            revision_exhausted: false,
        }
    }

    pub(crate) fn reconcile_documents(
        &mut self,
        keys: impl IntoIterator<Item = NativeDocumentKey>,
    ) {
        let keys = keys.into_iter().collect::<HashSet<_>>();
        self.documents.retain(|history| keys.contains(&history.key));
    }

    pub(crate) fn invalidate(&mut self, key: NativeDocumentKey) {
        self.documents.retain(|history| history.key != key);
    }

    pub(crate) fn invalidate_all(&mut self) {
        self.documents.clear();
    }

    #[cfg(test)]
    fn record_owned_step(&mut self, key: NativeDocumentKey, step: ExecutionId) {
        let Some(revision) = self.allocate_revision() else {
            return;
        };
        let mut history = self.take_document(key).unwrap_or_else(|| DocumentHistory {
            key,
            revision,
            undo: VecDeque::new(),
            redo: VecDeque::new(),
        });
        history.redo.clear();
        if history.undo.len() == MAX_RETAINED_HISTORY_STEPS {
            history.undo.pop_front();
        }
        history.undo.push_back(step);
        history.revision = revision;
        self.retain_document(history);
    }

    #[cfg(test)]
    fn record_no_step_execution(&mut self, key: NativeDocumentKey) {
        let Some(mut history) = self.take_document(key) else {
            return;
        };
        if history.redo.is_empty() {
            self.retain_document(history);
            return;
        }
        let Some(revision) = self.allocate_revision() else {
            return;
        };
        history.redo.clear();
        history.revision = revision;
        self.retain_document(history);
    }

    #[cfg(test)]
    fn prepare(&self, key: NativeDocumentKey, direction: HistoryDirection) -> Option<HistoryClaim> {
        let history = self.documents.iter().find(|history| history.key == key)?;
        let expected_step = match direction {
            HistoryDirection::Undo => history.undo.back(),
            HistoryDirection::Redo => history.redo.back(),
        }
        .copied()?;
        Some(HistoryClaim {
            key,
            direction,
            expected_step,
            revision: history.revision,
        })
    }

    #[cfg(test)]
    fn complete(&mut self, claim: HistoryClaim, succeeded: bool) -> bool {
        if !succeeded {
            self.invalidate(claim.key);
            return false;
        }
        let Some(revision) = self.allocate_revision() else {
            return false;
        };
        let Some(mut history) = self.take_document(claim.key) else {
            return false;
        };
        if history.revision != claim.revision
            || history.top(claim.direction) != Some(claim.expected_step)
        {
            self.invalidate(claim.key);
            return false;
        }
        match claim.direction {
            HistoryDirection::Undo => {
                let step = history
                    .undo
                    .pop_back()
                    .expect("the claimed undo step exists");
                history.redo.push_back(step);
            }
            HistoryDirection::Redo => {
                let step = history
                    .redo
                    .pop_back()
                    .expect("the claimed redo step exists");
                history.undo.push_back(step);
            }
        }
        history.revision = revision;
        self.retain_document(history);
        true
    }

    #[cfg(test)]
    fn force_issued(&mut self, key: NativeDocumentKey) {
        self.invalidate(key);
    }

    #[cfg(test)]
    pub(crate) fn seed_owned_step(&mut self, key: NativeDocumentKey, value: u64) {
        self.record_owned_step(key, ExecutionId::from_raw(value));
    }

    #[cfg(test)]
    pub(crate) fn has_owned_step(&self, key: NativeDocumentKey) -> bool {
        self.prepare(key, HistoryDirection::Undo).is_some()
            || self.prepare(key, HistoryDirection::Redo).is_some()
    }

    #[cfg(test)]
    fn take_document(&mut self, key: NativeDocumentKey) -> Option<DocumentHistory> {
        let position = self
            .documents
            .iter()
            .position(|history| history.key == key)?;
        self.documents.remove(position)
    }

    #[cfg(test)]
    fn retain_document(&mut self, history: DocumentHistory) {
        if self.documents.len() == MAX_TRACKED_HISTORY_DOCUMENTS {
            self.documents.pop_front();
        }
        self.documents.push_back(history);
    }

    #[cfg(test)]
    fn allocate_revision(&mut self) -> Option<HistoryRevision> {
        if self.revision_exhausted {
            return None;
        }
        let revision = HistoryRevision(self.next_revision);
        let Some(next) = self.next_revision.checked_add(1) else {
            self.documents.clear();
            self.revision_exhausted = true;
            return None;
        };
        self.next_revision = next;
        Some(revision)
    }
}

#[cfg(test)]
impl ExecutionId {
    const fn from_raw(value: u64) -> Self {
        Self(value)
    }
}

#[cfg(test)]
impl DocumentHistory {
    fn top(&self, direction: HistoryDirection) -> Option<ExecutionId> {
        match direction {
            HistoryDirection::Undo => self.undo.back(),
            HistoryDirection::Redo => self.redo.back(),
        }
        .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_behind_an_unknown_barrier() {
        let history = HistoryLedger::new();

        assert_eq!(history.prepare(key(1, 101), HistoryDirection::Undo), None);
        assert_eq!(history.prepare(key(1, 101), HistoryDirection::Redo), None);
    }

    #[test]
    fn traverses_only_the_contiguous_owned_suffix() {
        let key = key(1, 101);
        let mut history = HistoryLedger::new();
        history.record_owned_step(key, step(1));
        history.record_owned_step(key, step(2));

        let undo_two = history.prepare(key, HistoryDirection::Undo).unwrap();
        assert_eq!(undo_two.expected_step, step(2));
        assert!(history.complete(undo_two, true));
        let undo_one = history.prepare(key, HistoryDirection::Undo).unwrap();
        assert_eq!(undo_one.expected_step, step(1));
        assert!(history.complete(undo_one, true));
        assert_eq!(history.prepare(key, HistoryDirection::Undo), None);

        let redo_one = history.prepare(key, HistoryDirection::Redo).unwrap();
        assert_eq!(redo_one.expected_step, step(1));
        assert!(history.complete(redo_one, true));
        let redo_two = history.prepare(key, HistoryDirection::Redo).unwrap();
        assert_eq!(redo_two.expected_step, step(2));
        assert!(history.complete(redo_two, true));
        assert_eq!(history.prepare(key, HistoryDirection::Redo), None);
    }

    #[test]
    fn new_owned_step_and_lisp_without_a_step_invalidate_redo() {
        let key = key(1, 101);
        let mut history = HistoryLedger::new();
        history.record_owned_step(key, step(1));
        let undo = history.prepare(key, HistoryDirection::Undo).unwrap();
        assert!(history.complete(undo, true));

        history.record_no_step_execution(key);
        assert_eq!(history.prepare(key, HistoryDirection::Redo), None);

        history.record_owned_step(key, step(2));
        assert_eq!(
            history
                .prepare(key, HistoryDirection::Undo)
                .unwrap()
                .expected_step,
            step(2)
        );
    }

    #[test]
    fn step_cap_discards_the_bottom_and_never_crosses_the_raised_barrier() {
        let key = key(1, 101);
        let mut history = HistoryLedger::new();
        for value in 1..=MAX_RETAINED_HISTORY_STEPS as u64 + 1 {
            history.record_owned_step(key, step(value));
        }

        for expected in (2..=MAX_RETAINED_HISTORY_STEPS as u64 + 1).rev() {
            let claim = history.prepare(key, HistoryDirection::Undo).unwrap();
            assert_eq!(claim.expected_step, step(expected));
            assert!(history.complete(claim, true));
        }
        assert_eq!(history.prepare(key, HistoryDirection::Undo), None);
    }

    #[test]
    fn document_cap_forgets_the_oldest_document_without_reconstructing_it() {
        let mut history = HistoryLedger::new();
        for value in 1..=MAX_TRACKED_HISTORY_DOCUMENTS as u64 + 1 {
            history.record_owned_step(key(value as usize, value as usize + 100), step(value));
        }

        let evicted = key(1, 101);
        assert_eq!(history.prepare(evicted, HistoryDirection::Undo), None);
        history.record_owned_step(evicted, step(999));
        assert_eq!(
            history
                .prepare(evicted, HistoryDirection::Undo)
                .unwrap()
                .expected_step,
            step(999)
        );
        assert_eq!(history.documents.len(), MAX_TRACKED_HISTORY_DOCUMENTS);
    }

    #[test]
    fn document_reconciliation_requires_the_exact_database_generation() {
        let original = key(1, 101);
        let retained = key(2, 102);
        let replacement = key(1, 201);
        let mut history = HistoryLedger::new();
        history.record_owned_step(original, step(1));
        history.record_owned_step(retained, step(2));

        history.reconcile_documents([replacement, retained]);

        assert_eq!(history.prepare(original, HistoryDirection::Undo), None);
        assert_eq!(history.prepare(replacement, HistoryDirection::Undo), None);
        assert!(history.prepare(retained, HistoryDirection::Undo).is_some());
    }

    #[test]
    fn forced_or_ambiguous_activity_clears_both_directions_before_a_result() {
        let key = key(1, 101);
        let mut history = HistoryLedger::new();
        history.record_owned_step(key, step(1));
        history.record_owned_step(key, step(2));
        let undo = history.prepare(key, HistoryDirection::Undo).unwrap();
        assert!(history.complete(undo, true));
        assert!(history.prepare(key, HistoryDirection::Undo).is_some());
        assert!(history.prepare(key, HistoryDirection::Redo).is_some());

        history.force_issued(key);

        assert_eq!(history.prepare(key, HistoryDirection::Undo), None);
        assert_eq!(history.prepare(key, HistoryDirection::Redo), None);
    }

    #[test]
    fn stale_claim_cannot_survive_invalidation_or_key_recreation() {
        let key = key(1, 101);
        let mut history = HistoryLedger::new();
        history.record_owned_step(key, step(1));
        let stale = history.prepare(key, HistoryDirection::Undo).unwrap();
        history.invalidate(key);
        history.record_owned_step(key, step(1));

        assert!(!history.complete(stale, true));
        assert_eq!(history.prepare(key, HistoryDirection::Undo), None);
    }

    #[test]
    fn cap_eviction_makes_an_earlier_claim_stale() {
        let key = key(1, 101);
        let mut history = HistoryLedger::new();
        for value in 1..=MAX_RETAINED_HISTORY_STEPS as u64 {
            history.record_owned_step(key, step(value));
        }
        let stale = history.prepare(key, HistoryDirection::Undo).unwrap();

        history.record_owned_step(key, step(999));

        assert!(!history.complete(stale, true));
        assert_eq!(history.prepare(key, HistoryDirection::Undo), None);
    }

    #[test]
    fn document_eviction_and_same_key_recreation_cannot_revive_a_claim() {
        let original = key(1, 101);
        let mut history = HistoryLedger::new();
        history.record_owned_step(original, step(1));
        let stale = history.prepare(original, HistoryDirection::Undo).unwrap();
        for value in 2..=MAX_TRACKED_HISTORY_DOCUMENTS as u64 + 1 {
            history.record_owned_step(key(value as usize, value as usize + 100), step(value));
        }
        assert_eq!(history.prepare(original, HistoryDirection::Undo), None);

        history.record_owned_step(original, step(2));

        assert!(!history.complete(stale, true));
        assert_eq!(history.prepare(original, HistoryDirection::Undo), None);
    }

    #[test]
    fn close_then_pointer_reuse_does_not_recreate_forgotten_knowledge() {
        let key = key(1, 101);
        let mut history = HistoryLedger::new();
        history.record_owned_step(key, step(1));

        history.reconcile_documents([]);
        history.reconcile_documents([key]);

        assert_eq!(history.prepare(key, HistoryDirection::Undo), None);
    }

    #[test]
    fn revision_exhaustion_clears_every_document_and_stays_fail_closed() {
        let key = key(1, 101);
        let mut history = HistoryLedger::new();
        history.record_owned_step(key, step(1));
        history.next_revision = u64::MAX;

        history.record_owned_step(key, step(2));
        history.record_owned_step(key, step(3));

        assert_eq!(history.prepare(key, HistoryDirection::Undo), None);
        assert!(history.revision_exhausted);
    }

    fn key(document_token: usize, database_token: usize) -> NativeDocumentKey {
        NativeDocumentKey {
            document_token,
            database_token,
        }
    }

    fn step(value: u64) -> ExecutionId {
        ExecutionId::from_raw(value)
    }
}
