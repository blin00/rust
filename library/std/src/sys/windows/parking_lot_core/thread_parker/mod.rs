#[path = "windows/mod.rs"]
mod imp;

pub use self::imp::{thread_yield, ThreadParker, UnparkHandle};
