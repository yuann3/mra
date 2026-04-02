use std::alloc::{Layout, alloc as std_alloc, dealloc as std_dealloc};

#[no_mangle]
pub extern "C" fn alloc(size: i32) -> i32 {
    let layout = Layout::from_size_align(size as usize, 1).unwrap();
    unsafe { std_alloc(layout) as i32 }
}

#[no_mangle]
pub extern "C" fn dealloc(ptr: i32, size: i32) {
    let layout = Layout::from_size_align(size as usize, 1).unwrap();
    unsafe { std_dealloc(ptr as *mut u8, layout) }
}

#[no_mangle]
pub extern "C" fn invoke(_ptr: i32, _len: i32) -> i64 {
    // Try to grow memory well beyond the limit.
    // Each page is 64 KiB, so 2048 pages = 128 MiB.
    // Keep growing until we hit the limit.
    loop {
        let result = core::arch::wasm32::memory_grow(0, 256); // 16 MiB at a time
        if result == usize::MAX {
            // memory.grow returned -1, meaning it failed
            // Trigger an unreachable trap to surface the error
            core::arch::wasm32::unreachable();
        }
    }
}
