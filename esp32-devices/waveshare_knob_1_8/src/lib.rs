#![no_std]

extern crate alloc;

pub mod bsp;

pub mod tasks {
    pub use common::tasks::profiler::*;
    pub use common::tasks::screensaver::*;
}
