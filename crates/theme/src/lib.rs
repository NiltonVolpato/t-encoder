#![no_std]

extern crate alloc;

use slint::ComponentHandle;

slint::include_modules!();

pub fn init<T>(_ui: &T)
where
    T: ComponentHandle + 'static,
    for<'a> Theme<'a>: slint::Global<'a, T>,
{
}

