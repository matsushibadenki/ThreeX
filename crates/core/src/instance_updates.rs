//! Bounded revision journal. Stores indices only, never duplicate Instance data.
use std::{collections::VecDeque, ops::Range, sync::Arc};
const MAX_REVISIONS: usize = 64;
const MAX_RANGES: usize = 4096;
const MAX_RANGES_PER_REVISION: usize = 1024;
#[derive(Clone, Debug)]
pub struct SceneStamp {
    pub(crate) identity: Arc<()>,
    pub(crate) revision: u64,
}
impl PartialEq for SceneStamp {
    fn eq(&self, other: &Self) -> bool {
        self.revision == other.revision && Arc::ptr_eq(&self.identity, &other.identity)
    }
}
impl Eq for SceneStamp {}
/// Commit stamp to a renderer only after all ranges were successfully enqueued.
#[derive(Debug)]
pub struct InstanceUpdates {
    pub stamp: SceneStamp,
    pub ranges: Vec<Range<usize>>,
    pub full: bool,
}
#[derive(Default)]
pub(crate) struct InstanceJournal {
    records: VecDeque<Record>,
    range_count: usize,
}
struct Record {
    revision: u64,
    full: bool,
    ranges: Vec<Range<usize>>,
}
fn merge(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    if ranges.len() <= 1 {
        return ranges;
    }
    ranges.sort_unstable_by_key(|r| r.start);
    let mut result: Vec<Range<usize>> = Vec::new();
    for range in ranges {
        if let Some(last) = result.last_mut()
            && range.start <= last.end
        {
            last.end = last.end.max(range.end);
            continue;
        }
        result.push(range);
    }
    result
}
/// Collect contiguous runs during writes, avoiding an O(K) index snapshot.
pub(crate) struct InstanceChanges {
    full: bool,
    ranges: Vec<Range<usize>>,
}
impl InstanceChanges {
    pub fn new(full: bool) -> Self {
        Self {
            full,
            ranges: Vec::new(),
        }
    }
    pub fn push(&mut self, index: usize) {
        if self.full {
            return;
        }
        if let Some(last) = self.ranges.last_mut()
            && last.end == index
        {
            last.end = index + 1;
            return;
        }
        self.ranges.push(index..index + 1);
        if self.ranges.len() > MAX_RANGES_PER_REVISION {
            self.full = true;
            self.ranges = Vec::new();
        }
    }
}
impl InstanceJournal {
    pub fn record(&mut self, revision: u64, changes: InstanceChanges) {
        let ranges = merge(changes.ranges);
        self.range_count += ranges.len();
        self.records.push_back(Record {
            revision,
            full: changes.full,
            ranges,
        });
        while self.records.len() > MAX_REVISIONS || self.range_count > MAX_RANGES {
            let old = self.records.pop_front().unwrap();
            self.range_count -= old.ranges.len();
        }
    }
    pub fn updates(
        &self,
        stamp: SceneStamp,
        previous: Option<&SceneStamp>,
        count: usize,
        force_full: bool,
    ) -> InstanceUpdates {
        let full = || InstanceUpdates {
            stamp: stamp.clone(),
            ranges: if count == 0 {
                Vec::new()
            } else {
                std::iter::once(0..count).collect()
            },
            full: true,
        };
        if force_full {
            return full();
        }
        let Some(previous) = previous else {
            return full();
        };
        if !Arc::ptr_eq(&stamp.identity, &previous.identity) || previous.revision > stamp.revision {
            return full();
        }
        if previous.revision == stamp.revision {
            return InstanceUpdates {
                stamp,
                ranges: Vec::new(),
                full: false,
            };
        }
        let Some(first) = self.records.front() else {
            return full();
        };
        if previous.revision < first.revision - 1 {
            return full();
        }
        let mut ranges = Vec::new();
        for record in &self.records {
            if record.revision > previous.revision {
                if record.full {
                    return full();
                }
                ranges.extend(record.ranges.iter().filter_map(|range| {
                    let end = range.end.min(count);
                    (range.start < end).then_some(range.start..end)
                }));
            }
        }
        InstanceUpdates {
            stamp,
            ranges: merge(ranges),
            full: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_fragmented_journal_falls_back_without_payload_snapshots() {
        let identity = Arc::new(());
        let stamp = |revision| SceneStamp {
            identity: identity.clone(),
            revision,
        };
        let mut journal = InstanceJournal::default();
        let mut fragmented = InstanceChanges::new(false);
        for index in (0..2050).step_by(2) {
            fragmented.push(index);
        }
        journal.record(1, fragmented);
        assert!(journal.updates(stamp(1), Some(&stamp(0)), 3000, false).full);
        assert_eq!(journal.range_count, 0);
        for revision in 2..=66 {
            let mut changes = InstanceChanges::new(false);
            for index in (0..160).step_by(2) {
                changes.push(index);
            }
            journal.record(revision, changes);
        }
        assert!(journal.records.len() <= MAX_REVISIONS);
        assert!(journal.range_count <= MAX_RANGES);
        assert!(
            journal
                .updates(stamp(66), Some(&stamp(1)), 3000, false)
                .full
        );
        assert!(
            !journal
                .updates(stamp(66), Some(&stamp(65)), 3000, false)
                .full
        );
        assert!(
            journal
                .updates(stamp(66), Some(&stamp(67)), 3000, false)
                .full
        );
    }
}
