use std::{
    ffi::{CStr, CString, c_char},
    panic::{UnwindSafe, catch_unwind},
};

#[unsafe(no_mangle)]
pub extern "C" fn kc_abi_version() -> u32 {
    1
}

fn response(call: impl FnOnce() -> String + UnwindSafe) -> *mut c_char {
    let result = catch_unwind(call).unwrap_or_else(|_| {
        know_code_core::error_json("internal_error", "native operation panicked")
    });
    CString::new(result).expect("JSON escapes NUL").into_raw()
}
unsafe fn input<'a>(value: *const c_char) -> Result<&'a str, String> {
    if value.is_null() {
        return Err(know_code_core::error_json(
            "invalid_request",
            "null argument",
        ));
    }
    unsafe { CStr::from_ptr(value) }
        .to_str()
        .map_err(|_| know_code_core::error_json("invalid_request", "arguments must be UTF-8"))
}
/// # Safety
/// Both arguments must be valid NUL-terminated UTF-8 strings for this call.
/// Free the returned owned string exactly once using kc_string_free.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kc_query_json(
    database: *const c_char,
    request: *const c_char,
) -> *mut c_char {
    response(|| match unsafe { (input(database), input(request)) } {
        (Ok(database), Ok(request)) => know_code_core::query_json(database, request),
        (Err(error), _) | (_, Err(error)) => error,
    })
}
/// # Safety
/// Request must be a valid NUL-terminated UTF-8 string for this call.
/// Free the returned owned string exactly once using kc_string_free.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kc_workspace_json(request: *const c_char) -> *mut c_char {
    response(|| match unsafe { input(request) } {
        Ok(request) => know_code_core::workspace_json(request),
        Err(error) => error,
    })
}
/// # Safety
/// The pointer must be null or an unfreed result of kc_query_json/kc_workspace_json.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kc_string_free(value: *mut c_char) {
    if !value.is_null() {
        drop(unsafe { CString::from_raw(value) });
    }
}
