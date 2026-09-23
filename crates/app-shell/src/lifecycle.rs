// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! Application lifecycle and navigation coordinator.

use alloc::boxed::Box;
use alloc::rc::{Rc, Weak};
use core::any::Any;
use core::cell::RefCell;

pub use theme::AppInfo;

/// Factory trait that each application implements to participate in the App Shell.
pub trait AppFactory {
    /// Returns metadata (name, icon, accent color) for the application.
    fn info(&self) -> AppInfo;

    /// Instantiates and activates the application window, passing the shell context.
    ///
    /// Returns an opaque `Box<dyn Any>` holding the component handle / resources alive.
    fn launch(&self, context: ShellContext) -> Box<dyn Any>;

    /// Clones this factory trait object.
    fn clone_box(&self) -> Box<dyn AppFactory>;
}

impl Clone for Box<dyn AppFactory> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

/// Context provided to an active application to interact with the shell lifecycle.
#[derive(Clone)]
pub struct ShellContext {
    shell: Weak<RefCell<AppShell>>,
}

impl ShellContext {
    /// Signals the shell that the current application wishes to exit.
    pub fn exit(&self) {
        if let Some(shell) = self.shell.upgrade() {
            AppShell::step(shell);
        }
    }

    /// Queues the specified application to run once upon the next transition.
    pub fn set_next_app(&self, factory: Box<dyn AppFactory>) {
        if let Some(shell) = self.shell.upgrade() {
            shell.borrow_mut().set_next_app(factory);
        }
    }
}

/// Headless application lifecycle coordinator.
///
/// Manages the active application instance and handles transitions between default
/// and temporary (run-once) applications without creating any windows of its own.
pub struct AppShell {
    default_app: Box<dyn AppFactory>,
    next_app: Option<Box<dyn AppFactory>>,
    active_app: Option<Box<dyn Any>>,
}

impl AppShell {
    /// Creates a new `AppShell` with the given default application factory.
    pub fn new(default_app: Box<dyn AppFactory>) -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self { default_app, next_app: None, active_app: None }))
    }

    /// Starts the shell lifecycle, launching the initial application.
    pub fn start(shell: &Rc<RefCell<Self>>) {
        Self::step(shell.clone());
    }

    /// Steps the lifecycle to the next application:
    ///
    /// 1. Drops the currently active application.
    /// 2. If `next_app` is set, consumes it and launches that application.
    /// 3. Otherwise, launches the `default_app`.
    pub fn step(shell: Rc<RefCell<Self>>) {
        let next_factory = {
            let mut borrow = shell.borrow_mut();
            borrow.active_app = None;
            borrow.next_app.take().unwrap_or_else(|| borrow.default_app.clone())
        };

        let context = ShellContext { shell: Rc::downgrade(&shell) };

        let instance = next_factory.launch(context);
        shell.borrow_mut().active_app = Some(instance);
    }

    /// Configures an application to run once upon the next lifecycle transition.
    pub fn set_next_app(&mut self, factory: Box<dyn AppFactory>) {
        self.next_app = Some(factory);
    }

    /// Clears any queued run-once application.
    pub fn clear_next_app(&mut self) {
        self.next_app = None;
    }

    /// Signals an external exit (e.g. user gesture or button long-press) for the active app.
    pub fn exit_active_app(shell: &Rc<RefCell<Self>>) {
        Self::step(shell.clone());
    }

    /// Returns `true` if an application instance is currently active.
    pub fn has_active_app(&self) -> bool {
        self.active_app.is_some()
    }
}
