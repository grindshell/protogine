//! ABI v1 for trusted plugins on x86_64-pc-windows-msvc.
//! Regenerate include/protogine_plugin.h with protogine-headergen.

use std::ffi::c_void;

pub const PG_ABI_VERSION: u32 = 1;
pub const PG_OK: u32 = 0;
pub const PG_INVALID_ARGUMENT: u32 = 1;
pub const PG_UNSUPPORTED: u32 = 2;
pub const PG_ERROR: u32 = 3;
pub const PG_BUFFER_TOO_SMALL: u32 = 4;
pub const PG_PANIC: u32 = 5;
pub const PG_CONTRACT_ERROR: u32 = 6;
pub const PG_LOG_INFO: u32 = 0;
pub const PG_LOG_WARN: u32 = 1;
pub const PG_LOG_ERROR: u32 = 2;
pub const PG_ERROR_CAPACITY: u32 = 1024;

/// UTF-8 bytes, without a terminator. NULL is permitted only for length zero.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PgStr {
    pub data: *const u8,
    pub len: u32,
}

/// Host-owned diagnostic storage. Only written and data[0..capacity] may change.
/// written excludes any terminator and must be <= capacity, including on failure.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PgError {
    pub data: *mut u8,
    pub capacity: u32,
    pub written: u32,
}

/// Host-owned read-only byte span. NULL is permitted only for length zero.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PgBytes {
    pub data: *const u8,
    pub len: u64,
}

/// Host-owned output. Do not change data/capacity or retain pointers after return.
/// On success, written <= capacity. On failure, written must be zero.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PgOutput {
    pub data: *mut u8,
    pub capacity: u64,
    pub written: u64,
}

pub type PgLog =
    Option<unsafe extern "C" fn(context: *mut c_void, level: u32, message: PgStr) -> u32>;

/// Valid only during the current host-initiated call on the runtime thread.
/// Do not retain this table, its context, or callback arguments. Reserved = 0.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PgHost {
    pub abi_version: u32,
    pub struct_size: u32,
    pub context: *mut c_void,
    pub log: PgLog,
    pub reserved: [u64; 2],
}

/// Successful init publishes a non-NULL plugin-owned instance. Failed init must
/// clean up its allocations and leave *instance NULL. No exception may unwind.
pub type PgInit = Option<
    unsafe extern "C" fn(
        host: *const PgHost,
        instance: *mut *mut c_void,
        error: *mut PgError,
    ) -> u32,
>;

/// Always consumes/frees the instance, even when reporting failure. Called once.
pub type PgShutdown = Option<
    unsafe extern "C" fn(host: *const PgHost, instance: *mut c_void, error: *mut PgError) -> u32,
>;

/// Synchronous batch computation. Join all workers before return. Inputs and
/// output/error buffers are disjoint and host-owned. Errors must leave instance
/// state unchanged; panic/contract faults discard the session. Never reenter Lua.
/// No automatic retry occurs. Byte encodings belong to the declared schema.
pub type PgCall = Option<
    unsafe extern "C" fn(
        host: *const PgHost,
        instance: *mut c_void,
        input: PgBytes,
        output: *mut PgOutput,
        error: *mut PgError,
    ) -> u32,
>;

/// Immutable declaration stored in the library until unload. IDs and schema IDs
/// are dotted lowercase identifiers; schema_version is nonzero. Reserved = 0.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PgFunction {
    pub struct_size: u32,
    pub schema_version: u32,
    pub id: PgStr,
    pub schema: PgStr,
    pub call: PgCall,
    pub reserved: [u64; 2],
}

/// Query copies this exact v1 layout into host storage. All function pointers
/// are required. functions is NULL iff function_count == 0. Flags/reserved = 0.
/// Query must not create an instance. Every declaration is validated before init.
/// ID/function storage must be readable, correctly aligned and immutable until
/// library unload; function_count must describe the actual accessible array.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PgPlugin {
    pub abi_version: u32,
    pub struct_size: u32,
    pub id: PgStr,
    pub functions: *const PgFunction,
    pub function_count: u32,
    pub flags: u32,
    pub init: PgInit,
    pub shutdown: PgShutdown,
    pub reserved: [u64; 2],
}

pub type PgQuery = unsafe extern "C" fn(
    requested_abi: u32,
    destination_size: u32,
    destination: *mut PgPlugin,
    error: *mut PgError,
) -> u32;

unsafe extern "C" {
    /// The only required exported symbol. Reject unsupported ABI/size before
    /// touching destination. Never write beyond destination_size or unwind.
    pub fn protogine_plugin_query(
        requested_abi: u32,
        destination_size: u32,
        destination: *mut PgPlugin,
        error: *mut PgError,
    ) -> u32;
}

/// Rust plugins can wrap each extern C entry's body with this shim. It cannot
/// catch abort-mode panics, foreign exceptions or invalid-pointer accesses.
/// Payload cleanup is also guarded. If it panics, the secondary panic payload
/// is intentionally leaked so its destructor cannot unwind through the caller.
pub fn catch_status(call: impl FnOnce() -> u32) -> u32 {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(call)) {
        Ok(status) => status,
        Err(payload) => {
            // Panic payloads can have arbitrary destructors, including ones
            // that panic with another payload whose destructor also panics.
            if let Err(secondary) =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(payload)))
            {
                std::mem::forget(secondary);
            }
            PG_PANIC
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn panic_shim_translates_and_preserves_statuses() {
        assert_eq!(catch_status(|| PG_ERROR), PG_ERROR);
        assert_eq!(catch_status(|| panic!("fixture panic")), PG_PANIC);
    }
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn v1_layout() {
        use std::mem::{align_of, offset_of, size_of};
        assert_eq!(size_of::<PgStr>(), 16);
        assert_eq!(size_of::<PgError>(), 16);
        assert_eq!(size_of::<PgBytes>(), 16);
        assert_eq!(size_of::<PgOutput>(), 24);
        assert_eq!(size_of::<PgHost>(), 40);
        assert_eq!(size_of::<PgFunction>(), 64);
        assert_eq!(size_of::<PgPlugin>(), 72);
        assert_eq!(align_of::<PgPlugin>(), 8);
        assert_eq!(offset_of!(PgPlugin, functions), 24);
        assert_eq!(offset_of!(PgPlugin, init), 40);
        assert_eq!(offset_of!(PgPlugin, reserved), 56);
        assert_eq!(offset_of!(PgFunction, call), 40);
        assert_eq!(offset_of!(PgHost, log), 16);
    }
}
