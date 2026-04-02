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

/// Escape a string for JSON string value (handles quotes, backslashes, control chars).
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < '\x20' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

#[no_mangle]
pub extern "C" fn invoke(ptr: i32, len: i32) -> i64 {
    let input = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let input_str = std::str::from_utf8(input).unwrap_or("{}");

    // Echo the raw input JSON string as content
    let escaped = json_escape(input_str);
    let output = format!(
        r#"{{"content":"{}","is_error":false}}"#,
        escaped
    );

    let output_bytes = output.as_bytes();
    let out_len = output_bytes.len();
    let out_ptr = alloc(out_len as i32);

    unsafe {
        std::ptr::copy_nonoverlapping(
            output_bytes.as_ptr(),
            out_ptr as *mut u8,
            out_len,
        );
    }

    ((out_ptr as i64) << 32) | (out_len as i64)
}
