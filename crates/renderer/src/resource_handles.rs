//! Process-local resource identities. These are neither persistent asset IDs nor IPC authorization.
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT_OWNER: AtomicU64 = AtomicU64::new(1);
fn owner() -> u64 {
    let mut current = NEXT_OWNER.load(Ordering::Relaxed);
    loop {
        let next = current
            .checked_add(1)
            .expect("resource owner identity exhausted");
        match NEXT_OWNER.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => return current,
            Err(actual) => current = actual,
        }
    }
}
/// Opaque identity scoped to one pool lifetime, including its device/cache epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceHandle {
    owner: u64,
    slot: u32,
    generation: u64,
}
enum State<T> {
    Vacant,
    Pending,
    Ready(T),
    Retired,
}
struct Slot<T> {
    generation: u64,
    state: State<T>,
}
/// Bounded slot storage. A pending reservation consumes capacity just like a ready resource.
/// Callers serialize mutation (e.g. on the Rust owner worker); this pool starts no jobs.
pub struct ResourcePool<T> {
    owner: u64,
    limit: u32,
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
}
impl<T> ResourcePool<T> {
    pub fn new(limit: u32) -> Self {
        Self {
            owner: owner(),
            limit,
            slots: Vec::new(),
            free: Vec::new(),
        }
    }
    /// Reserve before starting a job or allocating resources. None means no capacity.
    pub fn reserve(&mut self) -> Option<ResourceHandle> {
        let index = if let Some(index) = self.free.pop() {
            self.slots[index as usize].state = State::Pending;
            index
        } else {
            if self.slots.len() >= self.limit as usize {
                return None;
            }
            let index = self.slots.len() as u32;
            self.slots.push(Slot {
                generation: 1,
                state: State::Pending,
            });
            index
        };
        Some(ResourceHandle {
            owner: self.owner,
            slot: index,
            generation: self.slots[index as usize].generation,
        })
    }
    fn slot(&self, handle: ResourceHandle) -> Option<&Slot<T>> {
        if handle.owner != self.owner {
            return None;
        }
        self.slots
            .get(handle.slot as usize)
            .filter(|s| s.generation == handle.generation)
    }
    pub fn is_pending(&self, handle: ResourceHandle) -> bool {
        self.slot(handle)
            .is_some_and(|s| matches!(s.state, State::Pending))
    }
    pub fn get(&self, handle: ResourceHandle) -> Option<&T> {
        match &self.slot(handle)?.state {
            State::Ready(value) => Some(value),
            _ => None,
        }
    }
    /// Only the first result for a current pending reservation can be published.
    /// Return rejected values to the caller so stale completion cleanup is explicit.
    pub fn publish(&mut self, handle: ResourceHandle, value: T) -> Result<(), T> {
        if !self.is_pending(handle) {
            return Err(value);
        }
        self.slots[handle.slot as usize].state = State::Ready(value);
        Ok(())
    }
    /// Cancel pending work or drop a ready resource. Stale/foreign handles are harmless.
    pub fn release(&mut self, handle: ResourceHandle) -> bool {
        if !self
            .slot(handle)
            .is_some_and(|s| matches!(s.state, State::Pending | State::Ready(_)))
        {
            return false;
        }
        let slot = &mut self.slots[handle.slot as usize];
        if let Some(next) = slot.generation.checked_add(1) {
            slot.generation = next;
            slot.state = State::Vacant;
            self.free.push(handle.slot);
        } else {
            // Never wrap to a generation that an old handle could contain.
            slot.state = State::Retired;
        }
        true
    }
    /// Invalidate every job/handle before releasing resources. A new epoch permits slot reuse.
    pub fn clear(&mut self) {
        self.owner = owner();
        self.slots.clear();
        self.free.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reuse_rejects_old_reads_release_and_late_completion() {
        let mut pool = ResourcePool::new(1);
        let old = pool.reserve().unwrap();
        assert_eq!(pool.reserve(), None);
        pool.publish(old, 7).unwrap();
        assert_eq!(pool.get(old), Some(&7));
        assert!(pool.release(old));
        let new = pool.reserve().unwrap();
        assert_eq!(old.slot, new.slot);
        assert_ne!(old, new);
        assert_eq!(pool.get(old), None);
        assert!(!pool.release(old));
        assert_eq!(pool.publish(old, 99), Err(99));
        pool.publish(new, 8).unwrap();
        assert_eq!(pool.publish(new, 9), Err(9));
        assert_eq!(pool.get(new), Some(&8));
    }
    #[test]
    fn foreign_pool_replacement_and_clear_invalidate_pending_jobs() {
        let mut a = ResourcePool::new(2);
        let mut b = ResourcePool::new(2);
        let old = a.reserve().unwrap();
        let other = b.reserve().unwrap();
        assert_eq!(old.slot, other.slot);
        assert_eq!(old.generation, other.generation);
        assert_eq!(b.publish(old, 1), Err(1));
        assert!(!b.release(old));
        a.clear();
        let current = a.reserve().unwrap();
        assert_ne!(old, current);
        assert_eq!(a.publish(old, 2), Err(2));
        a.publish(current, 3).unwrap();
        assert_eq!(a.get(current), Some(&3));
        drop(a);
        let mut replacement = ResourcePool::new(2);
        replacement.reserve().unwrap();
        assert_eq!(replacement.publish(current, 4), Err(4));
        b.publish(other, 5).unwrap();
    }
    #[test]
    fn exhausted_generations_retire_slots_without_wrapping() {
        let mut pool = ResourcePool::new(2);
        let mut handle = pool.reserve().unwrap();
        pool.slots[handle.slot as usize].generation = u64::MAX;
        handle.generation = u64::MAX;
        pool.publish(handle, 1).unwrap();
        assert!(pool.release(handle));
        assert!(!pool.release(handle));
        let next = pool.reserve().unwrap();
        assert_ne!(next.slot, handle.slot);
        assert!(pool.reserve().is_none());
        assert_eq!(pool.publish(handle, 2), Err(2));
        pool.clear();
        assert!(pool.reserve().is_some());
        assert!(ResourcePool::<u32>::new(0).reserve().is_none());
    }
    #[test]
    fn release_clear_drop_and_stale_results_destroy_each_resource_once() {
        use std::{cell::Cell, rc::Rc};
        struct Value(Rc<Cell<usize>>);
        impl Drop for Value {
            fn drop(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }
        let drops = Rc::new(Cell::new(0));
        let mut pool = ResourcePool::new(2);
        let first = pool.reserve().unwrap();
        assert!(pool.publish(first, Value(drops.clone())).is_ok());
        assert!(pool.release(first));
        assert_eq!(drops.get(), 1);
        drop(pool.publish(first, Value(drops.clone())).err().unwrap());
        assert_eq!(drops.get(), 2);
        let second = pool.reserve().unwrap();
        assert!(pool.publish(second, Value(drops.clone())).is_ok());
        pool.clear();
        assert_eq!(drops.get(), 3);
        let third = pool.reserve().unwrap();
        assert!(pool.publish(third, Value(drops.clone())).is_ok());
        drop(pool);
        assert_eq!(drops.get(), 4);
    }
    #[test]
    fn pending_capacity_cancel_and_duplicate_jobs_cannot_replace_live_value() {
        let mut pool = ResourcePool::new(2);
        let cancelled = pool.reserve().unwrap();
        let other = pool.reserve().unwrap();
        assert!(pool.is_pending(cancelled));
        assert!(pool.get(cancelled).is_none());
        assert!(pool.reserve().is_none());
        assert!(pool.release(cancelled));
        let current = pool.reserve().unwrap();
        assert_eq!(current.slot, cancelled.slot);
        assert_eq!(pool.publish(cancelled, 99), Err(99));
        assert!(pool.is_pending(current));
        pool.publish(current, 7).unwrap();
        assert!(!pool.is_pending(current));
        assert_eq!(pool.publish(current, 8), Err(8));
        pool.publish(other, 9).unwrap();
        assert_eq!(pool.get(current), Some(&7));
        assert_eq!(pool.get(other), Some(&9));
    }
}
