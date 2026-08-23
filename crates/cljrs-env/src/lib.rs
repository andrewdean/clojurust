#![allow(clippy::result_large_err)]
#![allow(clippy::arc_with_non_send_sync)]

pub mod apply;
pub mod async_hook;
pub mod callback;
pub mod interrupt;
pub mod io_target;
pub mod dynamics;
pub mod env;
pub mod error;
pub mod gas;
pub mod gc_roots;
pub mod loader;
pub mod policy;
pub mod taps;
#[cfg(all(not(target_arch = "wasm32"), feature = "host-dependencies"))]
pub mod versioned;
#[cfg(all(not(target_arch = "wasm32"), not(feature = "host-dependencies")))]
#[path = "versioned_restricted.rs"]
pub mod versioned;

pub use async_hook::AsyncRuntime;

pub fn add(left: u64, right: u64) -> u64 {
    left + right
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let result = add(2, 2);
        assert_eq!(result, 4);
    }
}
