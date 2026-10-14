//! Pages that more than one owner holds: a memory file (memfd_create) and every place it's mapped,
//! or a MAP_SHARED mapping and the copies of it fork gives children. Each such page is counted, and
//! goes back to the free pages when the last owner lets go. Their page-table entries carry
//! paging::SHARED, so unmapping one lets go instead of freeing it.

use super::frame;
use alloc::collections::BTreeMap;
use spin::Mutex;

static COUNTS: Mutex<BTreeMap<u64, u32>> = Mutex::new(BTreeMap::new());

/// A new zeroed page with one owner (the caller).
pub fn alloc() -> Option<u64> {
    let page = frame::alloc()?;
    COUNTS.lock().insert(page, 1);
    Some(page)
}

/// One more owner.
pub fn hold(page: u64) {
    *COUNTS.lock().entry(page).or_insert(0) += 1;
}

/// One owner fewer; the last one frees it.
pub fn release(page: u64) {
    let mut counts = COUNTS.lock();
    let Some(n) = counts.get_mut(&page) else { return };
    *n -= 1;
    if *n == 0 {
        counts.remove(&page);
        drop(counts);
        frame::free(page);
    }
}
