//! System bindings for the succinct zkvm platform
//!
//! This module contains the facade (aka platform-specific) implementations of
//! OS level functionality for zkvm.
//!
//! This is all super highly experimental and not actually intended for
//! wide/production use yet, it's still all in the experimental category. This
//! will likely change over time.

const WORD_SIZE: usize = core::mem::size_of::<u32>();

pub mod alloc;
#[path = "../zkvm/args.rs"]
pub mod args;
#[path = "../zkvm/cmath.rs"]
pub mod cmath;
pub mod env;
#[path = "../pal/unsupported/fs.rs"]
pub mod fs;
#[path = "../pal/unsupported/io.rs"]
pub mod io;
#[path = "../pal/unsupported/net.rs"]
pub mod net;
#[path = "../sync/once/mod.rs"]
pub mod once;
pub mod os;
#[path = "../os_str/mod.rs"]
pub mod os_str;
#[path = "../pal/unix/path.rs"]
pub mod path;
#[path = "../pal/unsupported/pipe.rs"]
pub mod pipe;
#[path = "../pal/unsupported/process.rs"]
pub mod process;
pub mod stdio;
pub mod thread_local_key;
#[path = "../pal/unsupported/time.rs"]
pub mod time;

#[path = "../pal/unsupported/locks/mod.rs"]
pub mod locks;
#[path = "../pal/unsupported/thread.rs"]
pub mod thread;

#[path = "../pal/unsupported/thread_parking.rs"]
pub mod thread_parking;

mod abi;

use crate::io as std_io;

pub mod memchr {
    pub use core::slice::memchr::{memchr, memrchr};
}

// SAFETY: must be called only once during runtime initialization.
// NOTE: this is not guaranteed to run, for example when Rust code is called externally.
pub unsafe fn init(_argc: isize, _argv: *const *const u8, _sigpipe: u8) {}

// SAFETY: must be called only once during runtime cleanup.
// NOTE: this is not guaranteed to run, for example when the program aborts.
pub unsafe fn cleanup() {}

pub fn unsupported<T>() -> std_io::Result<T> {
    Err(unsupported_err())
}

pub fn unsupported_err() -> std_io::Error {
    std_io::const_io_error!(
        std_io::ErrorKind::Unsupported,
        "operation not supported on this platform",
    )
}

pub fn is_interrupted(_code: i32) -> bool {
    false
}

pub fn decode_error_kind(_code: i32) -> crate::io::ErrorKind {
    crate::io::ErrorKind::Uncategorized
}

pub fn abort_internal() -> ! {
    core::intrinsics::abort();
}

pub fn hashmap_random_keys() -> (u64, u64) {
    let mut buf = [0u8; 16];
    unsafe {
        abi::sys_rand(buf.as_mut_ptr(), buf.len());
    };

    let a = u64::from_le_bytes(buf[0..8].try_into().unwrap());
    let b = u64::from_le_bytes(buf[8..16].try_into().unwrap());
    (a, b)
}
