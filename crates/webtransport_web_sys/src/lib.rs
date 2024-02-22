//! This currently requires `RUSTFLAGS="--cfg=web_sys_unstable_apis"` to compile.

#[cfg(feature = "web_sys_unstable_apis")]
mod impl_;

#[cfg(feature = "web_sys_unstable_apis")]
pub use impl_::*;
