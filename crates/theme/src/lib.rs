#![no_std]

extern crate alloc;

use slint::ComponentHandle;

slint::include_modules!();

/// Configures default exit navigation for an application.
pub fn setup_navigation<T: ComponentHandle + 'static>(app: &T, on_exit: impl Fn() + 'static)
where
    for<'a> Navigation<'a>: slint::Global<'a, T>,
{
    Navigation::get(app).on_exit(on_exit);
}

