#![no_std]

slint::include_modules!();
use slint::ComponentHandle;

pub fn init<T>(_ui: &T)
where
    T: ComponentHandle + 'static,
    for<'a> Theme<'a>: slint::Global<'a, T>,
{
}
