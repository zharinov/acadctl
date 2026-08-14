use std::collections::{HashSet, VecDeque};

use crate::documents::NativeDocumentKey;

#[cfg(test)]
pub(crate) const DATABASE_OBJECT_MODIFIED: u16 =
    1 << crate::ffi::NativeDatabaseActivityKind::ObjectModified.repr;
pub(crate) const KNOWN_DATABASE_ACTIVITY: u16 =
    (1 << (crate::ffi::NativeDatabaseActivityKind::ProxyResurrectionCompleted.repr + 1)) - 1;

const EVENT_SUBJECT_UNRESOLVED: u8 = 1 << 0;
const EVENT_SUBJECT_FOREIGN: u8 = 1 << 1;
const CURRENT_CONTEXT_MISSING_OR_PARTIAL: u8 = 1 << 2;
const CURRENT_CONTEXT_FOREIGN: u8 = 1 << 3;
const ACTIVE_CONTEXT_MISSING_OR_PARTIAL: u8 = 1 << 4;
const ACTIVE_CONTEXT_FOREIGN: u8 = 1 << 5;
const CURRENT_AND_ACTIVE_DIFFER: u8 = 1 << 6;
const MALFORMED_EVENT: u8 = 1 << 7;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum HistoryEventKind {
    Invalid,
    CommandWillStart,
    CommandEnded,
    CommandCancelled,
    CommandFailed,
    LispWillStart,
    LispEnded,
    LispCancelled,
    SystemVariableWillChange,
    SystemVariableChanged,
    UndoAuto,
    UndoControl,
    UndoBegin,
    UndoEnd,
    UndoMark,
    UndoBack,
    UndoNumber,
    RedoNumber,
    UndoWriteBoundary,
    SubcommandsWillBeUndone,
    DocumentCreated,
    DocumentWillBeDestroyed,
    DocumentBecameCurrent,
    DocumentActivated,
    DatabaseWillBeDestroyed,
    DatabaseActivity,
    DatabaseReactorAttachFailed,
    DatabaseReactorDetachFailed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HistoryContext {
    pub(crate) document_token: usize,
    pub(crate) database_token: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NativeHistoryEvent {
    pub(crate) kind: HistoryEventKind,
    pub(crate) event_context: HistoryContext,
    pub(crate) current_context: HistoryContext,
    pub(crate) active_context: HistoryContext,
    pub(crate) argument0: i32,
    pub(crate) argument1: i32,
    pub(crate) database_activity: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HistoryObservationFailureKind {
    MalformedEvent,
    DatabaseReactorAttach,
    DatabaseReactorDetach,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HistoryObservationFailure {
    pub(crate) kind: HistoryObservationFailureKind,
    pub(crate) raw_subject: HistoryContext,
    pub(crate) native_status: i32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ActiveHistoryEvidence {
    event_kinds: u32,
    database_activity: u16,
    context_flags: u8,
}

impl NativeHistoryEvent {
    pub(crate) fn malformed(self) -> bool {
        let database_activity_is_malformed = if self.kind == HistoryEventKind::DatabaseActivity {
            self.database_activity == 0
                || self.database_activity & !u32::from(KNOWN_DATABASE_ACTIVITY) != 0
        } else {
            self.database_activity != 0
        };
        self.kind == HistoryEventKind::Invalid || database_activity_is_malformed
    }

    pub(crate) fn observation_failure(self) -> Option<HistoryObservationFailure> {
        let kind = if self.malformed() {
            HistoryObservationFailureKind::MalformedEvent
        } else {
            match self.kind {
                HistoryEventKind::DatabaseReactorAttachFailed => {
                    HistoryObservationFailureKind::DatabaseReactorAttach
                }
                HistoryEventKind::DatabaseReactorDetachFailed => {
                    HistoryObservationFailureKind::DatabaseReactorDetach
                }
                _ => return None,
            }
        };
        Some(HistoryObservationFailure {
            kind,
            raw_subject: self.event_context,
            native_status: self.argument0,
        })
    }
}

impl ActiveHistoryEvidence {
    pub(crate) fn record(
        &mut self,
        target: NativeDocumentKey,
        event_subject: Option<NativeDocumentKey>,
        event: NativeHistoryEvent,
    ) {
        self.event_kinds |= event.kind.bit();

        self.database_activity |= (event.database_activity as u16) & KNOWN_DATABASE_ACTIVITY;
        if event.malformed() {
            self.context_flags |= MALFORMED_EVENT;
        }

        self.context_flags |= resolved_context_flags(
            event_subject,
            target,
            EVENT_SUBJECT_UNRESOLVED,
            EVENT_SUBJECT_FOREIGN,
        );
        self.context_flags |= context_flags(
            event.current_context,
            target,
            CURRENT_CONTEXT_MISSING_OR_PARTIAL,
            CURRENT_CONTEXT_FOREIGN,
        );
        self.context_flags |= context_flags(
            event.active_context,
            target,
            ACTIVE_CONTEXT_MISSING_OR_PARTIAL,
            ACTIVE_CONTEXT_FOREIGN,
        );
        if event.current_context != event.active_context {
            self.context_flags |= CURRENT_AND_ACTIVE_DIFFER;
        }
    }
}

impl HistoryEventKind {
    const fn bit(self) -> u32 {
        1 << self as u8
    }

    pub(crate) const fn invalidates_provenance(self) -> bool {
        !matches!(self, Self::DocumentBecameCurrent | Self::DocumentActivated)
    }
}

impl HistoryContext {
    pub(crate) const fn native_key(self) -> Option<NativeDocumentKey> {
        if self.document_token == 0 || self.database_token == 0 {
            return None;
        }
        Some(NativeDocumentKey {
            document_token: self.document_token,
            database_token: self.database_token,
        })
    }
}

fn context_flags(
    context: HistoryContext,
    target: NativeDocumentKey,
    missing_flag: u8,
    foreign_flag: u8,
) -> u8 {
    match context.native_key() {
        Some(key) if key == target => 0,
        Some(_) => foreign_flag,
        None => missing_flag,
    }
}

fn resolved_context_flags(
    context: Option<NativeDocumentKey>,
    target: NativeDocumentKey,
    missing_flag: u8,
    foreign_flag: u8,
) -> u8 {
    match context {
        Some(key) if key == target => 0,
        Some(_) => foreign_flag,
        None => missing_flag,
    }
}

#[cfg(test)]
const MAX_TRACKED_DOCUMENT_GENERATIONS: usize = 32;
#[cfg(test)]
const MAX_OWNED_STEPS_PER_DOCUMENT_GENERATION: usize = 256;

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExecutionId(u64);

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProvenanceRevision(u64);

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
    revision: ProvenanceRevision,
}

pub(crate) struct HistoryProvenance {
    documents: VecDeque<DocumentProvenance>,
    #[cfg(test)]
    next_revision: u64,
    #[cfg(test)]
    revision_exhausted: bool,
}

struct DocumentProvenance {
    key: NativeDocumentKey,
    #[cfg(test)]
    revision: ProvenanceRevision,
    #[cfg(test)]
    undo_suffix: VecDeque<ExecutionId>,
    #[cfg(test)]
    redo_suffix: VecDeque<ExecutionId>,
}

impl HistoryProvenance {
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
        self.documents
            .retain(|provenance| keys.contains(&provenance.key));
    }

    pub(crate) fn invalidate(&mut self, key: NativeDocumentKey) {
        self.documents.retain(|provenance| provenance.key != key);
    }

    pub(crate) fn invalidate_all(&mut self) {
        self.documents.clear();
    }

    #[cfg(test)]
    fn record_owned_undo_step(&mut self, key: NativeDocumentKey, step: ExecutionId) {
        let Some(revision) = self.allocate_revision() else {
            return;
        };
        let mut provenance = self
            .take_document(key)
            .unwrap_or_else(|| DocumentProvenance {
                key,
                revision,
                undo_suffix: VecDeque::new(),
                redo_suffix: VecDeque::new(),
            });
        provenance.redo_suffix.clear();
        if provenance.undo_suffix.len() == MAX_OWNED_STEPS_PER_DOCUMENT_GENERATION {
            provenance.undo_suffix.pop_front();
        }
        provenance.undo_suffix.push_back(step);
        provenance.revision = revision;
        self.retain_document(provenance);
    }

    #[cfg(test)]
    fn record_execution_without_undo_step(&mut self, key: NativeDocumentKey) {
        let Some(mut provenance) = self.take_document(key) else {
            return;
        };
        if provenance.redo_suffix.is_empty() {
            self.retain_document(provenance);
            return;
        }
        let Some(revision) = self.allocate_revision() else {
            return;
        };
        provenance.redo_suffix.clear();
        provenance.revision = revision;
        self.retain_document(provenance);
    }

    #[cfg(test)]
    fn prepare(&self, key: NativeDocumentKey, direction: HistoryDirection) -> Option<HistoryClaim> {
        let provenance = self
            .documents
            .iter()
            .find(|provenance| provenance.key == key)?;
        let expected_step = match direction {
            HistoryDirection::Undo => provenance.undo_suffix.back(),
            HistoryDirection::Redo => provenance.redo_suffix.back(),
        }
        .copied()?;
        Some(HistoryClaim {
            key,
            direction,
            expected_step,
            revision: provenance.revision,
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
        let Some(mut provenance) = self.take_document(claim.key) else {
            return false;
        };
        if provenance.revision != claim.revision
            || provenance.top(claim.direction) != Some(claim.expected_step)
        {
            self.invalidate(claim.key);
            return false;
        }
        match claim.direction {
            HistoryDirection::Undo => {
                let step = provenance
                    .undo_suffix
                    .pop_back()
                    .expect("the claimed undo step exists");
                provenance.redo_suffix.push_back(step);
            }
            HistoryDirection::Redo => {
                let step = provenance
                    .redo_suffix
                    .pop_back()
                    .expect("the claimed redo step exists");
                provenance.undo_suffix.push_back(step);
            }
        }
        provenance.revision = revision;
        self.retain_document(provenance);
        true
    }

    #[cfg(test)]
    fn force_issued(&mut self, key: NativeDocumentKey) {
        self.invalidate(key);
    }

    #[cfg(test)]
    pub(crate) fn seed_owned_step(&mut self, key: NativeDocumentKey, value: u64) {
        self.record_owned_undo_step(key, ExecutionId::from_raw(value));
    }

    #[cfg(test)]
    pub(crate) fn has_owned_step(&self, key: NativeDocumentKey) -> bool {
        self.prepare(key, HistoryDirection::Undo).is_some()
            || self.prepare(key, HistoryDirection::Redo).is_some()
    }

    #[cfg(test)]
    fn take_document(&mut self, key: NativeDocumentKey) -> Option<DocumentProvenance> {
        let position = self
            .documents
            .iter()
            .position(|provenance| provenance.key == key)?;
        self.documents.remove(position)
    }

    #[cfg(test)]
    fn retain_document(&mut self, provenance: DocumentProvenance) {
        if self.documents.len() == MAX_TRACKED_DOCUMENT_GENERATIONS {
            self.documents.pop_front();
        }
        self.documents.push_back(provenance);
    }

    #[cfg(test)]
    fn allocate_revision(&mut self) -> Option<ProvenanceRevision> {
        if self.revision_exhausted {
            return None;
        }
        let revision = ProvenanceRevision(self.next_revision);
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
impl DocumentProvenance {
    fn top(&self, direction: HistoryDirection) -> Option<ExecutionId> {
        match direction {
            HistoryDirection::Undo => self.undo_suffix.back(),
            HistoryDirection::Redo => self.redo_suffix.back(),
        }
        .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_evidence_is_a_constant_size_idempotent_summary() {
        let target = key(1, 101);
        let mut evidence = ActiveHistoryEvidence::default();
        let event = native_event(HistoryEventKind::CommandWillStart, target);

        for _ in 0..100_000 {
            evidence.record(target, Some(target), event);
        }

        assert_eq!(
            evidence.event_kinds,
            HistoryEventKind::CommandWillStart.bit()
        );
        assert_eq!(evidence.database_activity, 0);
        assert_eq!(evidence.context_flags, 0);
        assert!(std::mem::size_of::<ActiveHistoryEvidence>() <= 8);
    }

    #[test]
    fn active_evidence_preserves_every_database_activity_class() {
        let target = key(1, 101);
        let mut evidence = ActiveHistoryEvidence::default();
        let mut event = native_event(HistoryEventKind::DatabaseActivity, target);
        event.database_activity = u32::from(KNOWN_DATABASE_ACTIVITY);

        evidence.record(target, Some(target), event);

        assert_eq!(evidence.database_activity, KNOWN_DATABASE_ACTIVITY);
        assert_eq!(evidence.context_flags, 0);
    }

    #[test]
    fn active_evidence_latches_context_and_malformed_event_flags() {
        let target = key(1, 101);
        let foreign = key(2, 102);
        let mut evidence = ActiveHistoryEvidence::default();
        let mut event = native_event(HistoryEventKind::DatabaseActivity, target);
        event.event_context = HistoryContext {
            document_token: foreign.document_token,
            database_token: foreign.database_token,
        };
        event.current_context = HistoryContext {
            document_token: target.document_token,
            database_token: 0,
        };
        event.active_context = HistoryContext {
            document_token: foreign.document_token,
            database_token: foreign.database_token,
        };
        event.database_activity = 1 << 31;

        evidence.record(target, Some(foreign), event);

        assert_eq!(
            evidence.context_flags,
            EVENT_SUBJECT_FOREIGN
                | CURRENT_CONTEXT_MISSING_OR_PARTIAL
                | ACTIVE_CONTEXT_FOREIGN
                | CURRENT_AND_ACTIVE_DIFFER
                | MALFORMED_EVENT
        );
    }

    #[test]
    fn starts_behind_an_unknown_barrier() {
        let provenance = HistoryProvenance::new();

        assert_eq!(
            provenance.prepare(key(1, 101), HistoryDirection::Undo),
            None
        );
        assert_eq!(
            provenance.prepare(key(1, 101), HistoryDirection::Redo),
            None
        );
    }

    #[test]
    fn traverses_only_the_contiguous_owned_suffix() {
        let key = key(1, 101);
        let mut provenance = HistoryProvenance::new();
        provenance.record_owned_undo_step(key, step(1));
        provenance.record_owned_undo_step(key, step(2));

        let undo_two = provenance.prepare(key, HistoryDirection::Undo).unwrap();
        assert_eq!(undo_two.expected_step, step(2));
        assert!(provenance.complete(undo_two, true));
        let undo_one = provenance.prepare(key, HistoryDirection::Undo).unwrap();
        assert_eq!(undo_one.expected_step, step(1));
        assert!(provenance.complete(undo_one, true));
        assert_eq!(provenance.prepare(key, HistoryDirection::Undo), None);

        let redo_one = provenance.prepare(key, HistoryDirection::Redo).unwrap();
        assert_eq!(redo_one.expected_step, step(1));
        assert!(provenance.complete(redo_one, true));
        let redo_two = provenance.prepare(key, HistoryDirection::Redo).unwrap();
        assert_eq!(redo_two.expected_step, step(2));
        assert!(provenance.complete(redo_two, true));
        assert_eq!(provenance.prepare(key, HistoryDirection::Redo), None);
    }

    #[test]
    fn new_owned_step_and_lisp_without_a_step_invalidate_redo() {
        let key = key(1, 101);
        let mut provenance = HistoryProvenance::new();
        provenance.record_owned_undo_step(key, step(1));
        let undo = provenance.prepare(key, HistoryDirection::Undo).unwrap();
        assert!(provenance.complete(undo, true));

        provenance.record_execution_without_undo_step(key);
        assert_eq!(provenance.prepare(key, HistoryDirection::Redo), None);

        provenance.record_owned_undo_step(key, step(2));
        assert_eq!(
            provenance
                .prepare(key, HistoryDirection::Undo)
                .unwrap()
                .expected_step,
            step(2)
        );
    }

    #[test]
    fn step_cap_discards_the_bottom_and_never_crosses_the_raised_barrier() {
        let key = key(1, 101);
        let mut provenance = HistoryProvenance::new();
        for value in 1..=MAX_OWNED_STEPS_PER_DOCUMENT_GENERATION as u64 + 1 {
            provenance.record_owned_undo_step(key, step(value));
        }

        for expected in (2..=MAX_OWNED_STEPS_PER_DOCUMENT_GENERATION as u64 + 1).rev() {
            let claim = provenance.prepare(key, HistoryDirection::Undo).unwrap();
            assert_eq!(claim.expected_step, step(expected));
            assert!(provenance.complete(claim, true));
        }
        assert_eq!(provenance.prepare(key, HistoryDirection::Undo), None);
    }

    #[test]
    fn document_cap_forgets_the_oldest_document_without_reconstructing_it() {
        let mut provenance = HistoryProvenance::new();
        for value in 1..=MAX_TRACKED_DOCUMENT_GENERATIONS as u64 + 1 {
            provenance
                .record_owned_undo_step(key(value as usize, value as usize + 100), step(value));
        }

        let evicted = key(1, 101);
        assert_eq!(provenance.prepare(evicted, HistoryDirection::Undo), None);
        provenance.record_owned_undo_step(evicted, step(999));
        assert_eq!(
            provenance
                .prepare(evicted, HistoryDirection::Undo)
                .unwrap()
                .expected_step,
            step(999)
        );
        assert_eq!(provenance.documents.len(), MAX_TRACKED_DOCUMENT_GENERATIONS);
    }

    #[test]
    fn document_reconciliation_requires_the_exact_database_generation() {
        let original = key(1, 101);
        let retained = key(2, 102);
        let replacement = key(1, 201);
        let mut provenance = HistoryProvenance::new();
        provenance.record_owned_undo_step(original, step(1));
        provenance.record_owned_undo_step(retained, step(2));

        provenance.reconcile_documents([replacement, retained]);

        assert_eq!(provenance.prepare(original, HistoryDirection::Undo), None);
        assert_eq!(
            provenance.prepare(replacement, HistoryDirection::Undo),
            None
        );
        assert!(
            provenance
                .prepare(retained, HistoryDirection::Undo)
                .is_some()
        );
    }

    #[test]
    fn forced_or_ambiguous_activity_clears_both_directions_before_a_result() {
        let key = key(1, 101);
        let mut provenance = HistoryProvenance::new();
        provenance.record_owned_undo_step(key, step(1));
        provenance.record_owned_undo_step(key, step(2));
        let undo = provenance.prepare(key, HistoryDirection::Undo).unwrap();
        assert!(provenance.complete(undo, true));
        assert!(provenance.prepare(key, HistoryDirection::Undo).is_some());
        assert!(provenance.prepare(key, HistoryDirection::Redo).is_some());

        provenance.force_issued(key);

        assert_eq!(provenance.prepare(key, HistoryDirection::Undo), None);
        assert_eq!(provenance.prepare(key, HistoryDirection::Redo), None);
    }

    #[test]
    fn stale_claim_cannot_survive_invalidation_or_key_recreation() {
        let key = key(1, 101);
        let mut provenance = HistoryProvenance::new();
        provenance.record_owned_undo_step(key, step(1));
        let stale = provenance.prepare(key, HistoryDirection::Undo).unwrap();
        provenance.invalidate(key);
        provenance.record_owned_undo_step(key, step(1));

        assert!(!provenance.complete(stale, true));
        assert_eq!(provenance.prepare(key, HistoryDirection::Undo), None);
    }

    #[test]
    fn cap_eviction_makes_an_earlier_claim_stale() {
        let key = key(1, 101);
        let mut provenance = HistoryProvenance::new();
        for value in 1..=MAX_OWNED_STEPS_PER_DOCUMENT_GENERATION as u64 {
            provenance.record_owned_undo_step(key, step(value));
        }
        let stale = provenance.prepare(key, HistoryDirection::Undo).unwrap();

        provenance.record_owned_undo_step(key, step(999));

        assert!(!provenance.complete(stale, true));
        assert_eq!(provenance.prepare(key, HistoryDirection::Undo), None);
    }

    #[test]
    fn document_eviction_and_same_key_recreation_cannot_revive_a_claim() {
        let original = key(1, 101);
        let mut provenance = HistoryProvenance::new();
        provenance.record_owned_undo_step(original, step(1));
        let stale = provenance
            .prepare(original, HistoryDirection::Undo)
            .unwrap();
        for value in 2..=MAX_TRACKED_DOCUMENT_GENERATIONS as u64 + 1 {
            provenance
                .record_owned_undo_step(key(value as usize, value as usize + 100), step(value));
        }
        assert_eq!(provenance.prepare(original, HistoryDirection::Undo), None);

        provenance.record_owned_undo_step(original, step(2));

        assert!(!provenance.complete(stale, true));
        assert_eq!(provenance.prepare(original, HistoryDirection::Undo), None);
    }

    #[test]
    fn close_then_pointer_reuse_does_not_recreate_forgotten_knowledge() {
        let key = key(1, 101);
        let mut provenance = HistoryProvenance::new();
        provenance.record_owned_undo_step(key, step(1));

        provenance.reconcile_documents([]);
        provenance.reconcile_documents([key]);

        assert_eq!(provenance.prepare(key, HistoryDirection::Undo), None);
    }

    #[test]
    fn revision_exhaustion_clears_every_document_and_stays_fail_closed() {
        let key = key(1, 101);
        let mut provenance = HistoryProvenance::new();
        provenance.record_owned_undo_step(key, step(1));
        provenance.next_revision = u64::MAX;

        provenance.record_owned_undo_step(key, step(2));
        provenance.record_owned_undo_step(key, step(3));

        assert_eq!(provenance.prepare(key, HistoryDirection::Undo), None);
        assert!(provenance.revision_exhausted);
    }

    fn key(document_token: usize, database_token: usize) -> NativeDocumentKey {
        NativeDocumentKey {
            document_token,
            database_token,
        }
    }

    fn native_event(kind: HistoryEventKind, key: NativeDocumentKey) -> NativeHistoryEvent {
        let context = HistoryContext {
            document_token: key.document_token,
            database_token: key.database_token,
        };
        NativeHistoryEvent {
            kind,
            event_context: context,
            current_context: context,
            active_context: context,
            argument0: 0,
            argument1: 0,
            database_activity: 0,
        }
    }

    fn step(value: u64) -> ExecutionId {
        ExecutionId::from_raw(value)
    }
}
