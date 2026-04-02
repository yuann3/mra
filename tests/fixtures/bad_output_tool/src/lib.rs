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
    // Return non-JSON bytes
    let output = b"this is not json";
    let out_len = output.len();
    let out_ptr = alloc(out_len as i32);

    unsafe {
        std::ptr::copy_nonoverlapping(output.as_ptr(), out_ptr as *mut u8, out_len);
    }

    ((out_ptr as i64) << 32) | (out_len as i64)
}
