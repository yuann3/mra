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
    loop {
        // Spin forever — should be killed by epoch interruption
        core::hint::black_box(());
    }
}
