//! The pending events behind `sheltersim.events.queue.EventQueue`, without Python. It mirrors the
//! Python queue's structures one for one, a repeated identity from the constructor included, so
//! that `validate` refuses it the same way. Slots live by serial number, which stays when an event
//! is replaced. A slot has a row, or only its Python object, which the PyO3 crate's `QueueStore`
//! keeps by the same serial.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::EventRow;

/// The queue's order of an event: (due minute, priority, sequence).
pub type Key = (i64, i64, i64);

/// One pending event.
#[derive(Clone, Debug)]
pub struct Slot {
    pub event_id: Arc<str>,
    pub key: Key,
    /// None while the event is only a Python object no rule has read.
    pub row: Option<Arc<EventRow>>,
}

#[derive(Clone, Debug, Default)]
pub struct Queue {
    slots: HashMap<u64, Slot>,
    /// In the order they were scheduled, which `==` compares.
    insertion: Vec<u64>,
    /// In the order of `pending()`.
    ordered: Vec<(Key, u64)>,
    by_id: HashMap<Arc<str>, u64>,
    sequences: HashSet<i64>,
    next_serial: u64,
    /// Only the constructor can add a repeated identity.
    maybe_repeated: bool,
    /// Changes whenever an event comes, goes or is replaced: what is cached of the pending
    /// events holds while it is the same.
    generation: u64,
}

impl Queue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.insertion.len()
    }

    pub fn is_empty(&self) -> bool {
        self.insertion.is_empty()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn insert_ordered(&mut self, key: Key, serial: u64) {
        // As bisect.insort: after the entries with an equal key.
        let at = self.ordered.partition_point(|(other, _)| *other <= key);
        self.ordered.insert(at, (key, serial));
    }

    fn delete_ordered(&mut self, key: Key) {
        // As bisect_left: the first entry with this key.
        let at = self.ordered.partition_point(|(other, _)| *other < key);
        self.ordered.remove(at);
    }

    /// Adds an event, whatever is pending: its serial.
    pub fn add(&mut self, event_id: Arc<str>, key: Key, row: Option<Arc<EventRow>>) -> u64 {
        let serial = self.next_serial;
        self.next_serial += 1;
        self.by_id.entry(event_id.clone()).or_insert(serial);
        self.sequences.insert(key.2);
        self.insertion.push(serial);
        self.slots.insert(serial, Slot { event_id, key, row });
        self.insert_ordered(key, serial);
        self.generation += 1;
        serial
    }

    /// `add` of an event the constructor gives, which may repeat an identity: `validate` then
    /// looks.
    pub fn add_loaded(&mut self, event_id: Arc<str>, key: Key) -> u64 {
        if self.by_id.contains_key(&event_id) || self.sequences.contains(&key.2) {
            self.maybe_repeated = true;
        }
        self.add(event_id, key, None)
    }

    /// Takes out the first scheduled event of this ID: its serial and slot.
    pub fn take_slot(&mut self, event_id: &str) -> Option<(u64, Slot)> {
        let position = self.insertion.iter().position(|serial| &*self.slots[serial].event_id == event_id)?;
        let serial = self.insertion.remove(position);
        let slot = self.slots.remove(&serial)?;
        self.by_id.remove(event_id);
        self.sequences.remove(&slot.key.2);
        self.delete_ordered(slot.key);
        self.generation += 1;
        Some((serial, slot))
    }

    /// `EventQueue.schedule` of a row: 0, or 1 for a pending ID, 2 for a pending sequence.
    pub fn schedule_row(&mut self, row: EventRow) -> u8 {
        if self.by_id.contains_key(&*row.event_id) {
            return 1;
        }
        if self.sequences.contains(&row.sequence) {
            return 2;
        }
        let key = (row.due_minute, row.priority, row.sequence);
        self.add(row.event_id.clone(), key, Some(Arc::new(row)));
        0
    }

    /// The first scheduled event of this ID, as `EventQueue.replace` finds it.
    pub fn first_scheduled(&self, event_id: &str) -> Option<u64> {
        self.insertion.iter().copied().find(|serial| &*self.slots[serial].event_id == event_id)
    }

    /// A new order for an event, which has no row until one is read again.
    pub fn rekey(&mut self, serial: u64, key: Key) {
        let (event_id, current) = {
            let slot = &self.slots[&serial];
            (slot.event_id.clone(), slot.key)
        };
        self.delete_ordered(current);
        self.insert_ordered(key, serial);
        if let Some(slot) = self.slots.get_mut(&serial) {
            slot.key = key;
            slot.row = None;
        }
        self.by_id.insert(event_id, serial);
        self.generation += 1;
    }

    /// The slot of a serial that is pending.
    pub fn slot(&self, serial: u64) -> &Slot {
        &self.slots[&serial]
    }

    /// Keeps the row read of a pending event's object.
    pub fn set_row(&mut self, serial: u64, row: Arc<EventRow>) {
        if let Some(slot) = self.slots.get_mut(&serial) {
            slot.row = Some(row);
        }
    }

    pub fn serial_of(&self, event_id: &str) -> Option<u64> {
        self.by_id.get(event_id).copied()
    }

    /// Whether this event is pending.
    pub fn has_event(&self, event_id: &str) -> bool {
        self.by_id.contains_key(event_id)
    }

    pub fn has_sequence(&self, sequence: i64) -> bool {
        self.sequences.contains(&sequence)
    }

    /// The pending events in queue order.
    pub fn ordered(&self) -> &[(Key, u64)] {
        &self.ordered
    }

    /// How many of `ordered` are due through `minute`.
    pub fn due_end(&self, minute: i64) -> usize {
        self.ordered.partition_point(|(key, _)| key.0 <= minute)
    }

    /// The serials in the order they were scheduled.
    pub fn insertion(&self) -> &[u64] {
        &self.insertion
    }

    /// The IDs of the pending events in queue order, through `minute` if given.
    pub fn pending_ids(&self, minute: Option<i64>) -> Vec<Arc<str>> {
        let end = match minute {
            Some(minute) => self.due_end(minute),
            None => self.ordered.len(),
        };
        self.ordered[..end].iter().map(|(_, serial)| self.slots[serial].event_id.clone()).collect()
    }

    /// 0, or 1 for a repeated ID, 2 for a repeated sequence.
    pub fn validate(&self) -> u8 {
        if !self.maybe_repeated {
            return 0;
        }
        let ids: HashSet<&Arc<str>> = self.slots.values().map(|slot| &slot.event_id).collect();
        if ids.len() != self.slots.len() {
            return 1;
        }
        let sequences: HashSet<i64> = self.slots.values().map(|slot| slot.key.2).collect();
        if sequences.len() != self.slots.len() {
            return 2;
        }
        0
    }
}
