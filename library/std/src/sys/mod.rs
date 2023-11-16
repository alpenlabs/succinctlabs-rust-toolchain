/// The PAL (platform abstraction layer) contains platform-specific abstractions
/// for implementing the features in the other submodules, e.g. UNIX file
/// descriptors.
mod pal;

mod personality;

pub mod cmath;
pub mod os_str;
pub mod path;
pub mod sync;
#[allow(dead_code)]
#[allow(unused_imports)]
pub mod thread_local;

cfg_if::cfg_if! {
    if #[cfg(unix)] {
        mod unix;
        pub use self::unix::*;
    } else if #[cfg(windows)] {
        mod windows;
        pub use self::windows::*;
    } else if #[cfg(target_os = "solid_asp3")] {
        mod solid;
        pub use self::solid::*;
    } else if #[cfg(target_os = "hermit")] {
        mod hermit;
        pub use self::hermit::*;
    } else if #[cfg(target_os = "wasi")] {
        mod wasi;
        pub use self::wasi::*;
    } else if #[cfg(target_family = "wasm")] {
        mod wasm;
        pub use self::wasm::*;
    } else if #[cfg(target_os = "xous")] {
        mod xous;
        pub use self::xous::*;
    } else if #[cfg(target_os = "uefi")] {
        mod uefi;
        pub use self::uefi::*;
    } else if #[cfg(all(target_vendor = "fortanix", target_env = "sgx"))] {
        mod sgx;
        pub use self::sgx::*;
    } else if #[cfg(target_os = "zkvm")] {
        mod zkvm;
        pub use self::zkvm::*;
    } else {
        mod unsupported;
        pub use self::unsupported::*;
    }
}

// FIXME(117276): remove this, move feature implementations into individual
//                submodules.
pub use pal::*;
