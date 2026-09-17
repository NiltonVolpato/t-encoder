#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use core::any::Any;
use slint::ComponentHandle;

slint::include_modules!();

pub trait AppFactory {
    fn info(&self) -> AppInfo;
    fn launch(&self, on_exit: Box<dyn Fn() + 'static>) -> Box<dyn Any>;
}

pub fn init<T>(_ui: &T)
where
    T: ComponentHandle + 'static,
    for<'a> Theme<'a>: slint::Global<'a, T>,
{
}

