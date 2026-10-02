//! K11: a one-row change must not allocate in proportion to loaded output rows.

mod support;

use ivm_dd::{Dd, Engine, Frontier, SourceChange};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct CountingAllocator;

static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCATED: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() && COUNTING.load(Ordering::Relaxed) {
            ALLOCATED.fetch_add(layout.size(), Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc_zeroed(layout);
        if !ptr.is_null() && COUNTING.load(Ordering::Relaxed) {
            ALLOCATED.fetch_add(layout.size(), Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_ptr = System.realloc(ptr, layout, new_size);
        if !new_ptr.is_null() && COUNTING.load(Ordering::Relaxed) {
            ALLOCATED.fetch_add(new_size, Ordering::Relaxed);
        }
        new_ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
    }
}

fn insert(rel: u32, row: Vec<i64>) -> SourceChange {
    SourceChange { rel, row, w: 1 }
}

fn one_change_bytes(loaded: i64) -> usize {
    let program = support::program("0_access");
    let mut dd = Dd::install(&program).unwrap();
    let mut load = vec![insert(1, vec![10, 100])];
    load.extend((0..loaded).map(|person| insert(0, vec![person, 10])));
    dd.settle(Frontier { changes: load }).unwrap();

    ALLOCATED.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::SeqCst);
    dd.settle(Frontier { changes: vec![insert(2, vec![-1, 7])] }).unwrap();
    COUNTING.store(false, Ordering::SeqCst);
    ALLOCATED.load(Ordering::Relaxed)
}

#[test]
fn k11_one_row_change_allocation_is_independent_of_loaded_size() {
    let small = one_change_bytes(1_000);
    let large = one_change_bytes(100_000);
    assert!(small > 0, "allocator counted no bytes");
    assert!(large <= small * 4, "one-row settle allocated {small} bytes at 1e3 loaded, {large} at 1e5");
}
