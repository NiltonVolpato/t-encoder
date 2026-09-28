// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! Headless Application Shell: lifecycle coordination and system services.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod feedback;
pub mod lifecycle;
pub mod perf;
pub mod profile;
pub mod time;

pub use feedback::Feedback;
pub use lifecycle::{
    AppFactory, AppInfo, AppShell, AppShellPlatform, DefaultAppShellPlatform, ShellContext,
};
pub use perf::{FrameCycles, PerfSummary, PerfTracker};
pub use profile::{DEFAULT_TABLE_CAPACITY, PcSample, SampleTable};
pub use time::WallTime;

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;
    use alloc::rc::Rc;
    use alloc::string::String;
    use core::any::Any;
    use core::cell::RefCell;

    use super::*;

    #[derive(Clone)]
    struct MockAppFactory {
        name: &'static str,
        launched_count: Rc<RefCell<usize>>,
        on_launch_cb: Option<Rc<dyn Fn(ShellContext)>>,
    }

    impl MockAppFactory {
        fn new(name: &'static str) -> (Self, Rc<RefCell<usize>>) {
            let count = Rc::new(RefCell::new(0));
            (Self { name, launched_count: count.clone(), on_launch_cb: None }, count)
        }

        fn with_action(
            name: &'static str,
            action: impl Fn(ShellContext) + 'static,
        ) -> (Self, Rc<RefCell<usize>>) {
            let count = Rc::new(RefCell::new(0));
            (
                Self { name, launched_count: count.clone(), on_launch_cb: Some(Rc::new(action)) },
                count,
            )
        }
    }

    impl AppFactory for MockAppFactory {
        fn info(&self) -> AppInfo {
            AppInfo {
                name: self.name.into(),
                accent: slint::Color::from_argb_u8(255, 0, 128, 255),
                icon: slint::Image::default(),
            }
        }

        fn launch(&self, context: ShellContext) -> Box<dyn Any> {
            *self.launched_count.borrow_mut() += 1;
            if let Some(ref cb) = self.on_launch_cb {
                cb(context);
            }
            Box::new(self.name)
        }

        fn clone_box(&self) -> Box<dyn AppFactory> {
            Box::new(self.clone())
        }
    }

    struct TestPlatform {
        launched_names: Rc<RefCell<alloc::vec::Vec<String>>>,
    }

    impl AppShellPlatform for TestPlatform {
        fn on_app_launched(&self, info: &AppInfo) {
            self.launched_names.borrow_mut().push(info.name.as_str().into());
        }
    }

    #[test]
    fn test_single_app_lifecycle() {
        let (clock, launch_count) = MockAppFactory::new("Clock");
        let shell = AppShell::new(Box::new(DefaultAppShellPlatform), Box::new(clock));

        assert!(!shell.borrow().has_active_app());
        assert_eq!(*launch_count.borrow(), 0);

        // Start shell: boots standalone app
        AppShell::start(&shell);
        assert!(shell.borrow().has_active_app());
        assert_eq!(*launch_count.borrow(), 1);

        // External exit (e.g. gesture): restarts default app
        AppShell::exit_active_app(&shell);
        assert!(shell.borrow().has_active_app());
        assert_eq!(*launch_count.borrow(), 2);
    }

    #[test]
    fn test_platform_on_app_launched_hook() {
        let launched_names = Rc::new(RefCell::new(alloc::vec::Vec::new()));
        let platform = TestPlatform { launched_names: launched_names.clone() };
        let (clock, _) = MockAppFactory::new("Clock");
        let shell = AppShell::new(Box::new(platform), Box::new(clock));

        AppShell::start(&shell);
        assert_eq!(*launched_names.borrow(), alloc::vec!["Clock"]);

        AppShell::exit_active_app(&shell);
        assert_eq!(*launched_names.borrow(), alloc::vec!["Clock", "Clock"]);
    }

    #[test]
    fn test_launcher_next_app_lifecycle() {
        let (target_app, target_launch_count) = MockAppFactory::new("Simon");

        // Launcher sets next_app and exits immediately
        let target_clone = target_app.clone();
        let (launcher, launcher_launch_count) =
            MockAppFactory::with_action("Launcher", move |ctx| {
                ctx.set_next_app(Box::new(target_clone.clone()));
                ctx.exit();
            });

        let shell = AppShell::new(Box::new(DefaultAppShellPlatform), Box::new(launcher));

        // Starting shell launches Launcher, which queues Simon and exits, so Simon runs
        AppShell::start(&shell);
        assert_eq!(*launcher_launch_count.borrow(), 1);
        assert_eq!(*target_launch_count.borrow(), 1);

        // When Simon exits, shell returns to default app (Launcher)
        AppShell::exit_active_app(&shell);
        // Launcher ran again, which once more launched Simon
        assert_eq!(*launcher_launch_count.borrow(), 2);
        assert_eq!(*target_launch_count.borrow(), 2);
    }

    #[test]
    fn test_feedback_service() {
        feedback::clear();
        assert_eq!(feedback::try_receive(), None);

        feedback::signal(Feedback::DialStepForward);
        feedback::signal(Feedback::DialStepBackward);
        feedback::signal(Feedback::Click);
        feedback::signal(Feedback::Haptic);
        feedback::signal(Feedback::Tone { hz: 440, ms: 100 });

        assert_eq!(feedback::try_receive(), Some(Feedback::DialStepForward));
        assert_eq!(feedback::try_receive(), Some(Feedback::DialStepBackward));
        assert_eq!(feedback::try_receive(), Some(Feedback::Click));
        assert_eq!(feedback::try_receive(), Some(Feedback::Haptic));
        assert_eq!(feedback::try_receive(), Some(Feedback::Tone { hz: 440, ms: 100 }));
        assert_eq!(feedback::try_receive(), None);

        static WAKE_COUNT: core::sync::atomic::AtomicUsize =
            core::sync::atomic::AtomicUsize::new(0);
        fn test_waker() {
            WAKE_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        }

        feedback::register_waker(test_waker);
        feedback::signal(Feedback::Click);
        assert_eq!(WAKE_COUNT.load(core::sync::atomic::Ordering::Relaxed), 1);
        let _ = feedback::try_receive();
    }
}
