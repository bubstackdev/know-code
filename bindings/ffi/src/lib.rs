use std::{
    ffi::{CStr, CString, c_char},
    panic::{UnwindSafe, catch_unwind},
    sync::Arc,
};

#[unsafe(no_mangle)]
pub extern "C" fn kc_abi_version() -> u32 {
    1
}
#[unsafe(no_mangle)]
pub extern "C" fn kc_query_contract_version() -> u32 {
    know_code_core::execution::QUERY_CONTRACT_VERSION
}

/// One single-use core reservation. The caller may retain it through host parsing.
pub struct QueryReservation(Arc<know_code_core::admission::Reservation>);

#[unsafe(no_mangle)]
pub extern "C" fn kc_query_max_request_bytes() -> u32 {
    know_code_core::admission::MAX_REQUEST_BYTES as u32
}
/// # Safety
/// scope is null (account/default scope) or a valid NUL-terminated UTF-8 path.
/// out must be writable. On success returns null and writes an owned handle;
/// on failure writes null and returns owned JSON released with kc_string_free.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kc_query_reserve(
    scope: *const c_char,
    out: *mut *mut QueryReservation,
) -> *mut c_char {
    if out.is_null() {
        return response(|| {
            know_code_core::error_json("invalid_request", "null reservation output")
        });
    }
    unsafe { *out = std::ptr::null_mut() };
    let result = catch_unwind(|| {
        let scope = if scope.is_null() {
            know_code_core::admission::AdmissionScope::local()
        } else {
            let path = unsafe { input(scope) }?;
            know_code_core::admission::AdmissionScope::at(path)
        }
        .map_err(|e| know_code_core::error_json(e.code(), &e.to_string()))?;
        scope
            .try_reserve()
            .map_err(|e| know_code_core::error_json(e.code(), &e.to_string()))
    });
    match result {
        Ok(Ok(reservation)) => {
            unsafe { *out = Box::into_raw(Box::new(QueryReservation(Arc::new(reservation)))) };
            std::ptr::null_mut()
        }
        Ok(Err(error)) => response(|| error),
        Err(_) => {
            response(|| know_code_core::error_json("internal_error", "native reservation panicked"))
        }
    }
}
/// # Safety
/// Handle must be null or owned from kc_query_reserve, freed exactly once after
/// calls using it return. A result delivered to the caller is outside its charge.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kc_query_reservation_free(handle: *mut QueryReservation) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle) });
    }
}
/// # Safety
/// UTF-8 inputs follow kc_query_json. Reservation must remain live during this
/// call and may submit only once. Cancellation may be null or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kc_query_json_reserved(
    database: *const c_char,
    request: *const c_char,
    elapsed_ms: u64,
    cancellation: *const QueryCancel,
    reservation: *const QueryReservation,
) -> *mut c_char {
    let start = know_code_core::execution::QueryStart::with_elapsed_ms(elapsed_ms);
    response(
        || match unsafe { (input(database), input(request), reservation.as_ref()) } {
            (Ok(database), Ok(request), Some(reservation)) => {
                let reservation = Arc::clone(&reservation.0);
                let cancellation = unsafe { cancellation.as_ref() }.map(|c| c.0.clone());
                know_code_core::query_json_reserved(
                    database,
                    request,
                    start,
                    cancellation,
                    &reservation,
                )
            }
            (Err(error), _, _) | (_, Err(error), _) => error,
            (_, _, None) => know_code_core::error_json("invalid_request", "null query reservation"),
        },
    )
}

pub struct QueryCancel(know_code_core::execution::QueryCancellation);
#[unsafe(no_mangle)]
pub extern "C" fn kc_query_cancel_new() -> *mut QueryCancel {
    Box::into_raw(Box::new(QueryCancel(Default::default())))
}
/// # Safety
/// Handle must be live and cannot be freed concurrently with this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kc_query_cancel(handle: *const QueryCancel) {
    if let Some(handle) = unsafe { handle.as_ref() } {
        handle.0.cancel();
    }
}
/// # Safety
/// Handle must be null or owned from kc_query_cancel_new, freed exactly once
/// after all calls/cancellation callbacks using this handle have returned.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kc_query_cancel_free(handle: *mut QueryCancel) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle) });
    }
}
/// # Safety
/// Strings follow kc_query_json's contract. Handle must remain live while
/// this function clones it; never free concurrently. The clone owns its flag
/// independently through query completion and cursor cleanup.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kc_query_json_cancellable(
    database: *const c_char,
    request: *const c_char,
    elapsed_ms: u64,
    handle: *const QueryCancel,
) -> *mut c_char {
    let start = know_code_core::execution::QueryStart::with_elapsed_ms(elapsed_ms);
    response(
        || match unsafe { (input(database), input(request), handle.as_ref()) } {
            (Ok(database), Ok(request), Some(handle)) => know_code_core::query_json_cancellable(
                database,
                request,
                start,
                Some(handle.0.clone()),
            ),
            (Err(error), _, _) | (_, Err(error), _) => error,
            (_, _, None) => {
                know_code_core::error_json("invalid_request", "null cancellation handle")
            }
        },
    )
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
/// Same pointer/ownership contract as kc_query_json. elapsed_ms charges time
/// already spent in the host's asynchronous queue; it is not a clock epoch.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kc_query_json_elapsed(
    database: *const c_char,
    request: *const c_char,
    elapsed_ms: u64,
) -> *mut c_char {
    let start = know_code_core::execution::QueryStart::with_elapsed_ms(elapsed_ms);
    response(|| match unsafe { (input(database), input(request)) } {
        (Ok(database), Ok(request)) => know_code_core::query_json_started(database, request, start),
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
/// The pointer must be null or an unfreed result of a kc_*_json query/control call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kc_string_free(value: *mut c_char) {
    if !value.is_null() {
        drop(unsafe { CString::from_raw(value) });
    }
}
