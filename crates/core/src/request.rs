//! Persistent identifiers and bounded request receipts, independent of hosts.
use crate::{Document, DocumentError, PreparedTransaction};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
    str::FromStr,
};
macro_rules! identifier {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name([u8; 16]);
        impl $name {
            pub fn from_bytes(bytes: [u8; 16]) -> Option<Self> {
                (bytes != [0; 16]).then_some(Self(bytes))
            }
            pub fn bytes(self) -> [u8; 16] {
                self.0
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                for b in self.0 {
                    write!(f, "{b:02x}")?
                }
                Ok(())
            }
        }
        impl FromStr for $name {
            type Err = ();
            fn from_str(s: &str) -> Result<Self, ()> {
                if s.len() != 32 || !s.is_ascii() {
                    return Err(());
                }
                let mut b = [0; 16];
                for (i, v) in b.iter_mut().enumerate() {
                    *v = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|_| ())?
                }
                Self::from_bytes(b).ok_or(())
            }
        }
    };
}
identifier!(DocumentId);
identifier!(SessionId);
#[derive(Debug, PartialEq, Eq)]
pub enum RequestError {
    DocumentMismatch,
    InvalidSequence,
    SequenceGap,
    Expired,
    Mismatch,
    SessionLimit,
    SessionClosed,
    SessionUnknown,
    Edit(DocumentError),
}
#[derive(Clone)]
pub(crate) struct Receipt {
    pub session: SessionId,
    pub sequence: u64,
    pub kind: u8,
    pub expected: u64,
    pub revision: u64,
    pub commands: Vec<u8>,
    pub failure: Option<Box<DocumentError>>,
}
#[derive(Default)]
pub(crate) struct RequestLedger {
    pub sessions: BTreeMap<SessionId, u64>,
    pub closed: BTreeSet<SessionId>,
    pub receipts: VecDeque<Receipt>,
    pub bytes: usize,
}
pub enum RequestPreparation {
    Replay(u64),
    Pending(PreparedRequest),
}
pub struct PreparedRequest {
    edit: PreparedTransaction,
    receipt: Receipt,
    prior: u64,
    document: DocumentId,
}
impl PreparedRequest {
    pub fn revision(&self) -> u64 {
        self.receipt.revision
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    Active { sequence: u64 },
    Closed { sequence: u64 },
}
pub const MAX_SESSION_RECORDS: usize = 4096;
impl Document {
    pub fn session_status(&self, session: SessionId) -> Option<SessionStatus> {
        self.session_sequence(session).map(|sequence| {
            if self.ledger.closed.contains(&session) {
                SessionStatus::Closed { sequence }
            } else {
                SessionStatus::Active { sequence }
            }
        })
    }
    pub fn session_sequence(&self, session: SessionId) -> Option<u64> {
        self.ledger.sessions.get(&session).copied()
    }
    pub fn request_generation(&self) -> u128 {
        self.ledger.sessions.values().map(|&n| n as u128).sum()
    }
    pub fn document_id(&self) -> Option<DocumentId> {
        self.id
    }
    /// Hosts bind draft/legacy documents once, before exposing their persistent identity.
    pub fn bind_document_id(&mut self, id: DocumentId) -> Result<(), RequestError> {
        if self.id.is_some_and(|old| old != id) {
            return Err(RequestError::DocumentMismatch);
        }
        self.id = Some(id);
        Ok(())
    }
    pub fn prepare_request(
        &self,
        document: DocumentId,
        session: SessionId,
        sequence: u64,
        kind: u8,
        expected: u64,
        commands: &[u8],
    ) -> Result<RequestPreparation, RequestError> {
        if self.id != Some(document) {
            return Err(RequestError::DocumentMismatch);
        }
        if sequence == 0 {
            return Err(RequestError::InvalidSequence);
        }
        let prior = self.ledger.sessions.get(&session).copied().unwrap_or(0);
        if sequence <= prior {
            let Some(receipt) = self
                .ledger
                .receipts
                .iter()
                .find(|r| r.session == session && r.sequence == sequence)
            else {
                return Err(RequestError::Expired);
            };
            if receipt.kind != kind || receipt.expected != expected || receipt.commands != commands
            {
                return Err(RequestError::Mismatch);
            }
            if let Some(error) = &receipt.failure {
                return Err(RequestError::Edit(error.as_ref().clone()));
            }
            return Ok(RequestPreparation::Replay(receipt.revision));
        }
        if self.ledger.closed.contains(&session) {
            return Err(RequestError::SessionClosed);
        }
        if kind == 4 && prior == 0 {
            return Err(RequestError::SessionUnknown);
        }
        if prior.checked_add(1) != Some(sequence) {
            return Err(RequestError::SequenceGap);
        }
        if prior == 0
            && (self.ledger.sessions.len() - self.ledger.closed.len() >= 64
                || self.ledger.sessions.len() >= MAX_SESSION_RECORDS)
        {
            return Err(RequestError::SessionLimit);
        }
        let edit = match kind {
            1 => self.prepare_commit(expected, commands),
            2 if commands.is_empty() => self.prepare_undo(expected),
            3 if commands.is_empty() => self.prepare_redo(expected),
            4 if commands.is_empty() => self.prepare_commit(expected, &[]),
            _ => {
                return Err(RequestError::Edit(DocumentError::InvalidCommand(
                    "Unknown request kind".into(),
                )));
            }
        }
        .map_err(RequestError::Edit)?;
        let receipt = Receipt {
            session,
            sequence,
            kind,
            expected,
            revision: edit.revision(),
            commands: commands.to_vec(),
            failure: None,
        };
        Ok(RequestPreparation::Pending(PreparedRequest {
            edit,
            receipt,
            prior,
            document,
        }))
    }
    pub fn apply_request(&mut self, prepared: PreparedRequest) -> Result<u64, RequestError> {
        if self.id != Some(prepared.document) {
            return Err(RequestError::DocumentMismatch);
        }
        if self
            .ledger
            .sessions
            .get(&prepared.receipt.session)
            .copied()
            .unwrap_or(0)
            != prepared.prior
        {
            return Err(RequestError::Expired);
        }
        if prepared.prior == 0
            && (self.ledger.sessions.len() - self.ledger.closed.len() >= 64
                || self.ledger.sessions.len() >= MAX_SESSION_RECORDS)
        {
            return Err(RequestError::SessionLimit);
        }
        let revision = self
            .apply_prepared(prepared.edit)
            .map_err(RequestError::Edit)?;
        self.ledger
            .sessions
            .insert(prepared.receipt.session, prepared.receipt.sequence);
        if prepared.receipt.kind == 4 {
            self.ledger.closed.insert(prepared.receipt.session);
        }
        self.ledger.bytes += prepared.receipt.commands.len();
        self.ledger.receipts.push_back(prepared.receipt);
        while self.ledger.receipts.len() > 64 || self.ledger.bytes > 8 * 1024 * 1024 {
            self.ledger.bytes -= self.ledger.receipts.pop_front().unwrap().commands.len();
        }
        Ok(revision)
    }
}

/// Borrowed commit envelope. Group validation stages only deltas and bounded ledger metadata.
pub struct RequestCommit<'a> {
    pub document: DocumentId,
    pub session: SessionId,
    pub sequence: u64,
    pub expected: u64,
    pub commands: &'a [u8],
}
/// Opaque preparation tied to both scene state and durable request watermarks.
pub struct PreparedRequestGroup {
    document: DocumentId,
    generation: u128,
    edits: Option<crate::PreparedGroup>,
    receipts: Vec<Receipt>,
    results: Vec<(u64, bool)>,
}
impl PreparedRequestGroup {
    /// One (original revision, replayed) result per input, in input order.
    pub fn results(&self) -> &[(u64, bool)] {
        &self.results
    }
}
impl Document {
    /// At most 32 commits / 1 MiB. Any invalid envelope rejects the explicit group.
    /// Undo and Redo are barriers handled by prepare_request instead.
    pub fn prepare_request_group(
        &self,
        requests: &[RequestCommit<'_>],
    ) -> Result<PreparedRequestGroup, RequestError> {
        let mut builder = self.request_group_builder()?;
        for request in requests {
            builder.push(request)?;
        }
        Ok(builder.finish())
    }
    pub fn request_group_builder(&self) -> Result<RequestGroupBuilder<'_>, RequestError> {
        let document = self.id.ok_or(RequestError::DocumentMismatch)?;
        Ok(RequestGroupBuilder {
            source: self,
            document,
            scene: crate::document::CommitGroupBuilder::new(self),
            sessions: self.ledger.sessions.clone(),
            recent: self.ledger.receipts.iter().map(Recent::Existing).collect(),
            bytes: self.ledger.bytes,
            input_bytes: 0,
            receipts: Vec::new(),
            results: Vec::new(),
        })
    }
    pub fn check_request_group(&self, group: &PreparedRequestGroup) -> Result<(), RequestError> {
        if self.id != Some(group.document) {
            return Err(RequestError::DocumentMismatch);
        }
        if self.request_generation() != group.generation {
            return Err(RequestError::Expired);
        }
        if let Some(edits) = &group.edits {
            self.check_group(edits).map_err(RequestError::Edit)?;
        }
        Ok(())
    }
    pub fn apply_request_group(
        &mut self,
        group: PreparedRequestGroup,
    ) -> Result<Vec<(u64, bool)>, RequestError> {
        self.check_request_group(&group)?;
        if let Some(edits) = group.edits {
            self.apply_group(edits).map_err(RequestError::Edit)?;
        }
        for receipt in group.receipts {
            self.ledger
                .sessions
                .insert(receipt.session, receipt.sequence);
            self.ledger.bytes += receipt.commands.len();
            self.ledger.receipts.push_back(receipt);
            while self.ledger.receipts.len() > 64 || self.ledger.bytes > 8 * 1024 * 1024 {
                self.ledger.bytes -= self.ledger.receipts.pop_front().unwrap().commands.len();
            }
        }
        Ok(group.results)
    }
}

enum Recent<'a> {
    Existing(&'a Receipt),
    Pending(usize),
}
/// Incremental commit candidate preparation. Each push validates only the new request.
/// Rejected candidates leave this builder's accepted prefix intact. No Document mutation occurs.
pub struct RequestGroupBuilder<'a> {
    source: &'a Document,
    document: DocumentId,
    scene: crate::document::CommitGroupBuilder<'a>,
    sessions: BTreeMap<SessionId, u64>,
    recent: VecDeque<Recent<'a>>,
    bytes: usize,
    input_bytes: usize,
    receipts: Vec<Receipt>,
    results: Vec<(u64, bool)>,
}
impl<'a> RequestGroupBuilder<'a> {
    fn receipt(&self, recent: &Recent<'a>) -> &Receipt {
        match recent {
            Recent::Existing(r) => r,
            Recent::Pending(index) => &self.receipts[*index],
        }
    }
    pub fn push(&mut self, r: &RequestCommit<'_>) -> Result<(u64, bool), RequestError> {
        if self.results.len() >= 32 || self.input_bytes + r.commands.len() > 1024 * 1024 {
            return Err(RequestError::Edit(DocumentError::CommandTooLarge));
        }
        if r.document != self.document {
            return Err(RequestError::DocumentMismatch);
        }
        if r.sequence == 0 {
            return Err(RequestError::InvalidSequence);
        }
        let prior = self.sessions.get(&r.session).copied().unwrap_or(0);
        if r.sequence <= prior {
            let old = self
                .recent
                .iter()
                .map(|v| self.receipt(v))
                .find(|old| old.session == r.session && old.sequence == r.sequence)
                .ok_or(RequestError::Expired)?;
            if old.kind != 1 || old.expected != r.expected || old.commands != r.commands {
                return Err(RequestError::Mismatch);
            }
            if let Some(error) = &old.failure {
                return Err(RequestError::Edit(error.as_ref().clone()));
            }
            let result = (old.revision, true);
            self.results.push(result);
            self.input_bytes += r.commands.len();
            return Ok(result);
        }
        if self.source.ledger.closed.contains(&r.session) {
            return Err(RequestError::SessionClosed);
        }
        if prior.checked_add(1) != Some(r.sequence) {
            return Err(RequestError::SequenceGap);
        }
        if prior == 0
            && (self.sessions.len() - self.source.ledger.closed.len() >= 64
                || self.sessions.len() >= MAX_SESSION_RECORDS)
        {
            return Err(RequestError::SessionLimit);
        }
        let revision = self
            .scene
            .push(r.expected, r.commands)
            .map_err(RequestError::Edit)?;
        let index = self.receipts.len();
        self.receipts.push(Receipt {
            session: r.session,
            sequence: r.sequence,
            kind: 1,
            expected: r.expected,
            revision,
            commands: r.commands.to_vec(),
            failure: None,
        });
        self.sessions.insert(r.session, r.sequence);
        self.recent.push_back(Recent::Pending(index));
        self.bytes += r.commands.len();
        while self.recent.len() > 64 || self.bytes > 8 * 1024 * 1024 {
            let removed = self.recent.pop_front().unwrap();
            self.bytes -= self.receipt(&removed).commands.len();
        }
        self.input_bytes += r.commands.len();
        let result = (revision, false);
        self.results.push(result);
        Ok(result)
    }
    pub fn finish(self) -> PreparedRequestGroup {
        PreparedRequestGroup {
            document: self.document,
            generation: self.source.request_generation(),
            edits: if self.receipts.is_empty() {
                None
            } else {
                Some(self.scene.finish())
            },
            receipts: self.receipts,
            results: self.results,
        }
    }
}
impl PreparedRequestGroup {
    /// The accepted new commits only, ready for journaling without re-preparing the prefix.
    pub fn pending_commits(&self) -> impl Iterator<Item = (RequestCommit<'_>, u64)> {
        self.receipts.iter().map(|r| {
            (
                RequestCommit {
                    document: self.document,
                    session: r.session,
                    sequence: r.sequence,
                    expected: r.expected,
                    commands: &r.commands,
                },
                r.revision,
            )
        })
    }
}

/// The original rejection and observed revision, independent of the current Scene.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestRejection {
    pub error: DocumentError,
    pub revision: u64,
}
pub struct RecordedRejection<'a> {
    pub document: DocumentId,
    pub session: SessionId,
    pub sequence: u64,
    pub kind: u8,
    pub expected: u64,
    pub commands: &'a [u8],
    pub result: RequestRejection,
}
pub struct PreparedRejection {
    stamp: crate::SceneStamp,
    document: DocumentId,
    prior: u64,
    receipt: Receipt,
}
impl PreparedRejection {
    pub fn result(&self) -> RequestRejection {
        RequestRejection {
            error: self.receipt.failure.as_deref().unwrap().clone(),
            revision: self.receipt.revision,
        }
    }
}
/// Checkpoint/journal v4 bounded error codec. None means an unsupported/oversized value.
pub fn encode_request_error(error: &DocumentError) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    match error {
        DocumentError::RevisionConflict { current } => {
            bytes.push(1);
            bytes.extend(current.to_le_bytes());
        }
        DocumentError::CommandTooLarge => bytes.push(2),
        DocumentError::HistoryCapacity => bytes.push(3),
        DocumentError::RevisionExhausted => bytes.push(4),
        DocumentError::InvalidCommand(reason) => {
            if reason.len() > 1024 {
                return None;
            }
            bytes.push(5);
            bytes.extend(reason.as_bytes());
        }
        DocumentError::NothingToUndo => bytes.push(6),
        DocumentError::NothingToRedo => bytes.push(7),
    }
    Some(bytes)
}
pub fn decode_request_error(bytes: &[u8]) -> Option<DocumentError> {
    if bytes.is_empty() || bytes.len() > 1025 {
        return None;
    }
    Some(match bytes[0] {
        1 if bytes.len() == 9 => DocumentError::RevisionConflict {
            current: u64::from_le_bytes(bytes[1..].try_into().ok()?),
        },
        2 if bytes.len() == 1 => DocumentError::CommandTooLarge,
        3 if bytes.len() == 1 => DocumentError::HistoryCapacity,
        4 if bytes.len() == 1 => DocumentError::RevisionExhausted,
        5 => DocumentError::InvalidCommand(std::str::from_utf8(&bytes[1..]).ok()?.into()),
        6 if bytes.len() == 1 => DocumentError::NothingToUndo,
        7 if bytes.len() == 1 => DocumentError::NothingToRedo,
        _ => return None,
    })
}
impl Document {
    pub fn replay_rejection(
        &self,
        document: DocumentId,
        session: SessionId,
        sequence: u64,
        kind: u8,
        expected: u64,
        commands: &[u8],
    ) -> Result<Option<RequestRejection>, RequestError> {
        if self.id != Some(document) {
            return Err(RequestError::DocumentMismatch);
        }
        if let Some(r) = self
            .ledger
            .receipts
            .iter()
            .find(|r| r.session == session && r.sequence == sequence)
        {
            if r.kind != kind || r.expected != expected || r.commands != commands {
                return Err(RequestError::Mismatch);
            }
            return Ok(r.failure.as_ref().map(|e| RequestRejection {
                error: e.as_ref().clone(),
                revision: r.revision,
            }));
        }
        Ok(None)
    }
    /// Only editing failures of a fresh admissible envelope become durable outcomes.
    pub fn prepare_rejection(
        &self,
        document: DocumentId,
        session: SessionId,
        sequence: u64,
        kind: u8,
        expected: u64,
        commands: &[u8],
    ) -> Result<PreparedRejection, RequestError> {
        if self.id != Some(document) {
            return Err(RequestError::DocumentMismatch);
        }
        if sequence <= self.session_sequence(session).unwrap_or(0) {
            return Err(RequestError::Expired);
        }
        let error =
            match self.prepare_request(document, session, sequence, kind, expected, commands) {
                Err(RequestError::Edit(error)) => error,
                Err(other) => return Err(other),
                Ok(_) => return Err(RequestError::Mismatch),
            };
        if commands.len() > self.command_limit.min(256 * 1024)
            || !(1..=4).contains(&kind)
            || (kind != 1 && !commands.is_empty())
            || encode_request_error(&error).is_none()
        {
            return Err(RequestError::Edit(error));
        }
        Ok(PreparedRejection {
            stamp: self.scene.stamp(),
            document,
            prior: self.session_sequence(session).unwrap_or(0),
            receipt: Receipt {
                session,
                sequence,
                kind,
                expected,
                revision: self.revision(),
                commands: commands.to_vec(),
                failure: Some(Box::new(error)),
            },
        })
    }
    /// Restore the recorded outcome; never replace it with a newly computed error/success.
    pub fn prepare_recorded_rejection(
        &self,
        record: RecordedRejection<'_>,
    ) -> Result<PreparedRejection, RequestError> {
        let RecordedRejection {
            document,
            session,
            sequence,
            kind,
            expected,
            commands,
            result,
        } = record;
        if self.id != Some(document) {
            return Err(RequestError::DocumentMismatch);
        }
        if sequence <= self.session_sequence(session).unwrap_or(0) {
            return Err(RequestError::Expired);
        }
        match self.prepare_request(document, session, sequence, kind, expected, commands) {
            Ok(RequestPreparation::Pending(_)) | Err(RequestError::Edit(_)) => {}
            Err(other) => return Err(other),
            Ok(_) => return Err(RequestError::Expired),
        }
        if result.revision != self.revision()
            || commands.len() > self.command_limit.min(256 * 1024)
            || !(1..=4).contains(&kind)
            || (kind != 1 && !commands.is_empty())
            || encode_request_error(&result.error).is_none()
            || match &result.error {
                DocumentError::RevisionConflict { current } => {
                    *current != result.revision || expected == result.revision
                }
                _ => expected != result.revision,
            }
        {
            return Err(RequestError::Mismatch);
        }
        Ok(PreparedRejection {
            stamp: self.scene.stamp(),
            document,
            prior: self.session_sequence(session).unwrap_or(0),
            receipt: Receipt {
                session,
                sequence,
                kind,
                expected,
                revision: result.revision,
                commands: commands.to_vec(),
                failure: Some(Box::new(result.error)),
            },
        })
    }
    pub fn apply_rejection(
        &mut self,
        prepared: PreparedRejection,
    ) -> Result<RequestRejection, RequestError> {
        if self.id != Some(prepared.document) {
            return Err(RequestError::DocumentMismatch);
        }
        if self.session_sequence(prepared.receipt.session).unwrap_or(0) != prepared.prior {
            return Err(RequestError::Expired);
        }
        if !self.scene.is_current(Some(&prepared.stamp)) {
            return Err(RequestError::Edit(DocumentError::RevisionConflict {
                current: self.revision(),
            }));
        }
        if prepared.prior == 0
            && (self.ledger.sessions.len() - self.ledger.closed.len() >= 64
                || self.ledger.sessions.len() >= MAX_SESSION_RECORDS)
        {
            return Err(RequestError::SessionLimit);
        }
        let result = prepared.result();
        self.ledger
            .sessions
            .insert(prepared.receipt.session, prepared.receipt.sequence);
        self.ledger.bytes += prepared.receipt.commands.len();
        self.ledger.receipts.push_back(prepared.receipt);
        while self.ledger.receipts.len() > 64 || self.ledger.bytes > 8 * 1024 * 1024 {
            self.ledger.bytes -= self.ledger.receipts.pop_front().unwrap().commands.len();
        }
        Ok(result)
    }
}

#[cfg(test)]
mod group_tests {
    use super::*;
    fn draft() -> (Document, DocumentId, SessionId) {
        let mut d = Document::default();
        let id = DocumentId::from_bytes([1; 16]).unwrap();
        d.bind_document_id(id).unwrap();
        (d, id, SessionId::from_bytes([2; 16]).unwrap())
    }
    #[test]
    fn grouped_noops_guard_metadata_and_reject_invalid_groups_without_changes() {
        let (mut d, id, session) = draft();
        let make = |sequence| RequestCommit {
            document: id,
            session,
            sequence,
            expected: 0,
            commands: &[],
        };
        let group = d
            .prepare_request_group(&[make(1), make(2), make(1)])
            .unwrap();
        assert_eq!(group.results(), &[(0, false), (0, false), (0, true)]);
        let stale = d.prepare_request_group(&[make(1)]).unwrap();
        d.apply_request_group(group).unwrap();
        assert_eq!(d.revision(), 0);
        assert!(matches!(
            d.apply_request_group(stale),
            Err(RequestError::Expired)
        ));
        assert!(matches!(
            d.prepare_request_group(&[make(3), make(5)]),
            Err(RequestError::SequenceGap)
        ));
        assert_eq!(d.session_sequence(session), Some(2));
        assert!(matches!(
            d.prepare_request_group(&(0..33).map(|_| make(1)).collect::<Vec<_>>()),
            Err(RequestError::Edit(DocumentError::CommandTooLarge))
        ));
        let bytes = vec![0; 1024 * 1024 + 64];
        assert!(matches!(
            d.prepare_request_group(&[RequestCommit {
                commands: &bytes,
                ..make(3)
            }]),
            Err(RequestError::Edit(DocumentError::CommandTooLarge))
        ));
    }
    #[test]
    fn virtual_receipt_eviction_matches_sequential_requests() {
        let (mut d, id, session) = draft();
        for sequence in 1..=64 {
            let RequestPreparation::Pending(p) =
                d.prepare_request(id, session, sequence, 1, 0, &[]).unwrap()
            else {
                panic!()
            };
            d.apply_request(p).unwrap();
        }
        let input = |sequence| RequestCommit {
            document: id,
            session,
            sequence,
            expected: 0,
            commands: &[],
        };
        assert!(matches!(
            d.prepare_request_group(&[input(65), input(1)]),
            Err(RequestError::Expired)
        ));
        assert_eq!(d.session_sequence(session), Some(64));
        let group = d.prepare_request_group(&[input(65), input(2)]).unwrap();
        assert_eq!(
            d.apply_request_group(group).unwrap(),
            vec![(0, false), (0, true)]
        );
        let mut restored = Document::from_checkpoint(&d.checkpoint().unwrap()).unwrap();
        assert!(matches!(
            restored.prepare_request(id, session, 1, 1, 0, &[]),
            Err(RequestError::Expired)
        ));
        let group = restored.prepare_request_group(&[input(65)]).unwrap();
        assert_eq!(
            restored.apply_request_group(group).unwrap(),
            vec![(0, true)]
        );
    }
    #[test]
    fn group_preparation_is_bound_to_scene_identity_and_session_capacity() {
        let (mut d, id, session) = draft();
        let mut command = [0; 64];
        command[..4].copy_from_slice(&1u32.to_le_bytes());
        command[4..8].copy_from_slice(&1u32.to_le_bytes());
        let input = RequestCommit {
            document: id,
            session,
            sequence: 1,
            expected: 0,
            commands: &command,
        };
        let group = d.prepare_request_group(&[input]).unwrap();
        let mut other = Document::from_checkpoint(&d.checkpoint().unwrap()).unwrap();
        assert!(matches!(
            other.apply_request_group(group),
            Err(RequestError::Edit(DocumentError::RevisionConflict { .. }))
        ));
        assert_eq!(other.request_generation(), 0);
        for n in 1..=63 {
            let session = SessionId::from_bytes([n; 16]).unwrap();
            let group = d
                .prepare_request_group(&[RequestCommit {
                    document: id,
                    session,
                    sequence: 1,
                    expected: 0,
                    commands: &[],
                }])
                .unwrap();
            d.apply_request_group(group).unwrap();
        }
        let candidate = [64, 65].map(|n| RequestCommit {
            document: id,
            session: SessionId::from_bytes([n; 16]).unwrap(),
            sequence: 1,
            expected: 0,
            commands: &[],
        });
        assert!(matches!(
            d.prepare_request_group(&candidate),
            Err(RequestError::SessionLimit)
        ));
        assert_eq!(d.ledger.sessions.len(), 63);
        let mut wrong = RequestCommit {
            document: DocumentId::from_bytes([9; 16]).unwrap(),
            session,
            sequence: 2,
            expected: 0,
            commands: &[],
        };
        assert!(matches!(
            d.prepare_request_group(&[wrong]),
            Err(RequestError::DocumentMismatch)
        ));
        wrong = RequestCommit {
            document: id,
            session,
            sequence: 0,
            expected: 0,
            commands: &[],
        };
        assert!(matches!(
            d.prepare_request_group(&[wrong]),
            Err(RequestError::InvalidSequence)
        ));
    }
}

#[cfg(test)]
mod incremental_tests {
    use super::*;
    fn command(op: u32, id: u32, parent: u32) -> Vec<u8> {
        let mut b = [0; 64];
        b[..4].copy_from_slice(&op.to_le_bytes());
        b[4..8].copy_from_slice(&id.to_le_bytes());
        b[60..64].copy_from_slice(&parent.to_le_bytes());
        let mut bytes = b.to_vec();
        if op == 1 && parent != 0 {
            b[..4].copy_from_slice(&2u32.to_le_bytes());
            bytes.extend(b);
        }
        bytes
    }
    #[test]
    fn rejected_candidate_preserves_hierarchy_revision_receipts_and_history_prefix() {
        let mut d = Document::default();
        let id = DocumentId::from_bytes([11; 16]).unwrap();
        d.bind_document_id(id).unwrap();
        let session = SessionId::from_bytes([12; 16]).unwrap();
        let mut reference = Document::from_checkpoint(&d.checkpoint().unwrap()).unwrap();
        let parent = command(1, 1, 0);
        let child = command(1, 2, 1);
        let orphan = command(1, 3, 99);
        let cycle = command(2, 1, 2);
        let input = |sequence, expected, commands| RequestCommit {
            document: id,
            session,
            sequence,
            expected,
            commands,
        };
        let mut builder = d.request_group_builder().unwrap();
        assert_eq!(builder.push(&input(1, 0, &parent)).unwrap(), (1, false));
        assert!(builder.push(&input(2, 1, &orphan)).is_err());
        assert!(builder.push(&input(2, 0, &child)).is_err());
        assert_eq!(builder.push(&input(2, 1, &child)).unwrap(), (2, false));
        assert!(builder.push(&input(3, 2, &cycle)).is_err());
        assert_eq!(builder.push(&input(3, 2, &[])).unwrap(), (2, false));
        assert_eq!(builder.push(&input(1, 0, &parent)).unwrap(), (1, true));
        let prepared = builder.finish();
        assert_eq!(prepared.pending_commits().count(), 3);
        assert_eq!(d.revision(), 0);
        assert_eq!(d.request_generation(), 0);
        d.apply_request_group(prepared).unwrap();
        for r in [input(1, 0, &parent), input(2, 1, &child), input(3, 2, &[])] {
            let RequestPreparation::Pending(p) = reference
                .prepare_request(id, session, r.sequence, 1, r.expected, r.commands)
                .unwrap()
            else {
                panic!()
            };
            reference.apply_request(p).unwrap();
        }
        assert_eq!(d.checkpoint().unwrap(), reference.checkpoint().unwrap());
        d.undo(2).unwrap();
        assert!(d.scene().object(2).is_none());
        d.undo(3).unwrap();
        assert!(d.scene().is_empty());
    }
    #[test]
    fn rejected_bounds_and_mismatch_do_not_consume_candidate_capacity() {
        let mut d = Document::default();
        let id = DocumentId::from_bytes([13; 16]).unwrap();
        d.bind_document_id(id).unwrap();
        let session = SessionId::from_bytes([14; 16]).unwrap();
        let mut builder = d.request_group_builder().unwrap();
        let input = |sequence| RequestCommit {
            document: id,
            session,
            sequence,
            expected: 0,
            commands: &[],
        };
        builder.push(&input(1)).unwrap();
        let changed = command(1, 1, 0);
        assert!(matches!(
            builder.push(&RequestCommit {
                commands: &changed,
                ..input(1)
            }),
            Err(RequestError::Mismatch)
        ));
        let oversized = vec![0; 1024 * 1024 + 64];
        assert!(matches!(
            builder.push(&RequestCommit {
                commands: &oversized,
                ..input(2)
            }),
            Err(RequestError::Edit(DocumentError::CommandTooLarge))
        ));
        for sequence in 2..=32 {
            builder.push(&input(sequence)).unwrap();
        }
        assert!(matches!(
            builder.push(&input(33)),
            Err(RequestError::Edit(DocumentError::CommandTooLarge))
        ));
        let prepared = builder.finish();
        assert_eq!(prepared.results().len(), 32);
        d.apply_request_group(prepared).unwrap();
        assert_eq!(d.session_sequence(session), Some(32));
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    fn draft() -> (Document, DocumentId, SessionId) {
        let mut d = Document::default();
        let id = DocumentId::from_bytes([71; 16]).unwrap();
        d.bind_document_id(id).unwrap();
        (d, id, SessionId::from_bytes([72; 16]).unwrap())
    }
    fn apply(
        d: &mut Document,
        id: DocumentId,
        s: SessionId,
        n: u64,
        kind: u8,
    ) -> Result<u64, RequestError> {
        match d.prepare_request(id, s, n, kind, d.revision(), &[])? {
            RequestPreparation::Replay(r) => Ok(r),
            RequestPreparation::Pending(p) => d.apply_request(p),
        }
    }
    #[test]
    fn close_replays_old_results_but_rejects_new_edits_and_stale_plans() {
        let (mut d, id, s) = draft();
        assert_eq!(
            apply(&mut d, id, s, 1, 4),
            Err(RequestError::SessionUnknown)
        );
        apply(&mut d, id, s, 1, 1).unwrap();
        let original = d.content_hash();
        let group = d
            .prepare_request_group(&[RequestCommit {
                document: id,
                session: s,
                sequence: 2,
                expected: 0,
                commands: &[],
            }])
            .unwrap();
        let RequestPreparation::Pending(stale) = d.prepare_request(id, s, 2, 1, 0, &[]).unwrap()
        else {
            panic!()
        };
        apply(&mut d, id, s, 2, 4).unwrap();
        assert_eq!(
            d.session_status(s),
            Some(SessionStatus::Closed { sequence: 2 })
        );
        assert_eq!(d.content_hash(), original);
        assert_eq!(d.revision(), 0);
        assert!(matches!(d.apply_request(stale), Err(RequestError::Expired)));
        assert!(matches!(
            d.apply_request_group(group),
            Err(RequestError::Expired)
        ));
        for n in [3, 99] {
            assert!(matches!(
                d.prepare_request(id, s, n, 1, 0, &[]),
                Err(RequestError::SessionClosed)
            ));
        }
        assert!(matches!(
            d.prepare_request(id, s, 1, 1, 0, &[]),
            Ok(RequestPreparation::Replay(0))
        ));
        assert!(matches!(
            d.prepare_request(id, s, 2, 4, 0, &[]),
            Ok(RequestPreparation::Replay(0))
        ));
        assert!(matches!(
            d.prepare_request(id, s, 2, 1, 0, &[]),
            Err(RequestError::Mismatch)
        ));
        let mut d = Document::from_checkpoint(&d.checkpoint().unwrap()).unwrap();
        assert_eq!(apply(&mut d, id, s, 3, 1), Err(RequestError::SessionClosed));
        let mut builder = d.request_group_builder().unwrap();
        assert!(matches!(
            builder.push(&RequestCommit {
                document: id,
                session: s,
                sequence: 3,
                expected: 0,
                commands: &[]
            }),
            Err(RequestError::SessionClosed)
        ));
        let other = SessionId::from_bytes([73; 16]).unwrap();
        builder
            .push(&RequestCommit {
                document: id,
                session: other,
                sequence: 1,
                expected: 0,
                commands: &[],
            })
            .unwrap();
        d.apply_request_group(builder.finish()).unwrap();
    }
    #[test]
    fn closing_releases_active_slots_without_forgetting_bounded_tombstones() {
        let (mut d, id, first) = draft();
        apply(&mut d, id, first, 1, 1).unwrap();
        apply(&mut d, id, first, 2, 4).unwrap();
        for i in 2..=MAX_SESSION_RECORDS {
            let mut bytes = [0; 16];
            bytes[..8].copy_from_slice(&(i as u64).to_le_bytes());
            let s = SessionId::from_bytes(bytes).unwrap();
            apply(&mut d, id, s, 1, 1).unwrap();
            apply(&mut d, id, s, 2, 4).unwrap();
        }
        assert_eq!(d.ledger.sessions.len(), MAX_SESSION_RECORDS);
        assert!(matches!(
            d.prepare_request(id, first, 1, 1, 0, &[]),
            Err(RequestError::Expired)
        ));
        let other = SessionId::from_bytes([74; 16]).unwrap();
        assert_eq!(
            apply(&mut d, id, other, 1, 1),
            Err(RequestError::SessionLimit)
        );
        let restored = Document::from_checkpoint(&d.checkpoint().unwrap()).unwrap();
        assert_eq!(
            restored.session_status(first),
            Some(SessionStatus::Closed { sequence: 2 })
        );
        assert!(matches!(
            restored.prepare_request(id, first, 3, 1, 0, &[]),
            Err(RequestError::SessionClosed)
        ));
        assert!(matches!(
            restored.prepare_request(id, first, 1, 1, 0, &[]),
            Err(RequestError::Expired)
        ));
    }
    #[test]
    fn end_reclaims_active_capacity_and_rejected_closed_candidate_preserves_prefix() {
        let (mut d, id, s) = draft();
        apply(&mut d, id, s, 1, 1).unwrap();
        for n in 1..64 {
            let mut bytes = [0; 16];
            bytes[..8].copy_from_slice(&(n as u64).to_le_bytes());
            apply(&mut d, id, SessionId::from_bytes(bytes).unwrap(), 1, 1).unwrap();
        }
        let next = SessionId::from_bytes([77; 16]).unwrap();
        assert_eq!(
            apply(&mut d, id, next, 1, 1),
            Err(RequestError::SessionLimit)
        );
        apply(&mut d, id, s, 2, 4).unwrap();
        let mut builder = d.request_group_builder().unwrap();
        builder
            .push(&RequestCommit {
                document: id,
                session: next,
                sequence: 1,
                expected: 0,
                commands: &[],
            })
            .unwrap();
        assert!(matches!(
            builder.push(&RequestCommit {
                document: id,
                session: s,
                sequence: 3,
                expected: 0,
                commands: &[]
            }),
            Err(RequestError::SessionClosed)
        ));
        d.apply_request_group(builder.finish()).unwrap();
        assert_eq!(d.ledger.sessions.len(), 65);
        assert_eq!(d.ledger.closed.len(), 1);
        let restored = Document::from_checkpoint(&d.checkpoint().unwrap()).unwrap();
        assert_eq!(
            restored.session_status(s),
            Some(SessionStatus::Closed { sequence: 2 })
        );
        assert_eq!(
            restored.session_status(next),
            Some(SessionStatus::Active { sequence: 1 })
        );
    }
}

#[cfg(test)]
mod rejection_tests {
    use super::*;
    fn draft() -> (Document, DocumentId, SessionId) {
        let mut d = Document::default();
        let id = DocumentId::from_bytes([101; 16]).unwrap();
        d.bind_document_id(id).unwrap();
        (d, id, SessionId::from_bytes([102; 16]).unwrap())
    }
    #[test]
    fn rejection_replays_original_error_without_history_or_scene_changes() {
        let (mut d, id, s) = draft();
        let hash = d.content_hash();
        let p = d.prepare_rejection(id, s, 1, 2, 0, &[]).unwrap();
        let result = d.apply_rejection(p).unwrap();
        assert_eq!(
            result,
            RequestRejection {
                error: DocumentError::NothingToUndo,
                revision: 0
            }
        );
        assert_eq!(d.content_hash(), hash);
        assert_eq!(d.session_sequence(s), Some(1));
        let mut command = [0; 64];
        command[..4].copy_from_slice(&1u32.to_le_bytes());
        command[4..8].copy_from_slice(&1u32.to_le_bytes());
        d.commit(0, &command).unwrap();
        assert_eq!(
            d.replay_rejection(id, s, 1, 2, 0, &[]).unwrap(),
            Some(result.clone())
        );
        assert!(matches!(
            d.prepare_request(id, s, 1, 2, 0, &[]),
            Err(RequestError::Edit(DocumentError::NothingToUndo))
        ));
        assert_eq!(d.scene().len(), 1);
        let d = Document::from_checkpoint(&d.checkpoint().unwrap()).unwrap();
        assert_eq!(
            d.replay_rejection(id, s, 1, 2, 0, &[]).unwrap(),
            Some(result)
        );
        assert!(matches!(
            d.replay_rejection(id, s, 1, 3, 0, &[]),
            Err(RequestError::Mismatch)
        ));
    }
    #[test]
    fn rejection_codec_bounds_and_recorded_outcome_are_stable() {
        for error in [
            DocumentError::RevisionConflict { current: 4 },
            DocumentError::CommandTooLarge,
            DocumentError::HistoryCapacity,
            DocumentError::RevisionExhausted,
            DocumentError::InvalidCommand("bad / 不正 / 错误".into()),
            DocumentError::NothingToUndo,
            DocumentError::NothingToRedo,
        ] {
            assert_eq!(
                decode_request_error(&encode_request_error(&error).unwrap()),
                Some(error)
            );
        }
        for bytes in [
            vec![],
            vec![0],
            vec![8],
            vec![1],
            vec![6, 0],
            vec![5, 255],
            vec![5; 1026],
        ] {
            assert!(decode_request_error(&bytes).is_none());
        }
        assert!(encode_request_error(&DocumentError::InvalidCommand("a".repeat(1025))).is_none());
        let (mut d, id, s) = draft();
        let result = RequestRejection {
            error: DocumentError::InvalidCommand("older validator rejected this command".into()),
            revision: 0,
        };
        // Even if a later validator would accept an empty commit, replay restores the stored error.
        let p = d
            .prepare_recorded_rejection(RecordedRejection {
                document: id,
                session: s,
                sequence: 1,
                kind: 1,
                expected: 0,
                commands: &[],
                result: result.clone(),
            })
            .unwrap();
        assert_eq!(d.apply_rejection(p).unwrap(), result);
        assert_eq!(d.revision(), 0);
    }
    #[test]
    fn stale_rejection_does_not_consume_sequence_and_failed_close_stays_active() {
        let (mut d, id, s) = draft();
        let pending = d.prepare_rejection(id, s, 1, 2, 0, &[]).unwrap();
        let mut cmd = [0; 64];
        cmd[..4].copy_from_slice(&1u32.to_le_bytes());
        cmd[4..8].copy_from_slice(&1u32.to_le_bytes());
        d.commit(0, &cmd).unwrap();
        assert!(d.apply_rejection(pending).is_err());
        assert_eq!(d.session_sequence(s), None);
        let RequestPreparation::Pending(p) = d.prepare_request(id, s, 1, 1, 1, &[]).unwrap() else {
            panic!()
        };
        d.apply_request(p).unwrap();
        let p = d.prepare_rejection(id, s, 2, 4, 0, &[]).unwrap();
        d.apply_rejection(p).unwrap();
        assert_eq!(
            d.session_status(s),
            Some(SessionStatus::Active { sequence: 2 })
        );
        let RequestPreparation::Pending(p) = d.prepare_request(id, s, 3, 4, 1, &[]).unwrap() else {
            panic!()
        };
        d.apply_request(p).unwrap();
        assert_eq!(
            d.session_status(s),
            Some(SessionStatus::Closed { sequence: 3 })
        );
        assert!(Document::from_checkpoint(&d.checkpoint().unwrap()).is_ok());
    }
    #[test]
    fn metadata_only_interleaving_cannot_overfill_active_session_capacity() {
        let (mut d, id, _) = draft();
        for n in 1..64 {
            let mut b = [0; 16];
            b[..8].copy_from_slice(&(n as u64).to_le_bytes());
            let session = SessionId::from_bytes(b).unwrap();
            let RequestPreparation::Pending(p) =
                d.prepare_request(id, session, 1, 1, 0, &[]).unwrap()
            else {
                panic!()
            };
            d.apply_request(p).unwrap();
        }
        let a = SessionId::from_bytes([121; 16]).unwrap();
        let b = SessionId::from_bytes([122; 16]).unwrap();
        let c = SessionId::from_bytes([123; 16]).unwrap();
        let pa = d.prepare_rejection(id, a, 1, 2, 0, &[]).unwrap();
        let pb = d.prepare_rejection(id, b, 1, 2, 0, &[]).unwrap();
        let RequestPreparation::Pending(pc) = d.prepare_request(id, c, 1, 1, 0, &[]).unwrap()
        else {
            panic!()
        };
        d.apply_rejection(pa).unwrap();
        assert_eq!(d.apply_rejection(pb), Err(RequestError::SessionLimit));
        assert_eq!(d.apply_request(pc), Err(RequestError::SessionLimit));
        assert_eq!(d.session_sequence(b), None);
        assert_eq!(d.session_sequence(c), None);
        assert!(Document::from_checkpoint(&d.checkpoint().unwrap()).is_ok());
    }
}
