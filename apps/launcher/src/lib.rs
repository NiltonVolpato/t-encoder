// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::any::Any;
use core::cell::RefCell;
use slint::ComponentHandle;

use theme::{AppFactory, AppInfo};

slint::include_modules!();

pub struct LauncherManager {
    launcher: Option<LauncherApp>,
    factories: Vec<Box<dyn AppFactory>>,
    current_index: usize,
    active_app: Option<Box<dyn Any>>,
}

impl LauncherManager {
    pub fn new() -> Rc<RefCell<Self>> {
        let factories: Vec<Box<dyn AppFactory>> = alloc::vec![
            Box::new(app_clock::ClockAppFactory::new()),
            Box::new(app_pomodoro::PomodoroAppFactory::new()),
            Box::new(app_macropad::MacropadAppFactory::new()),
            Box::new(app_simon::SimonAppFactory::new()),
            Box::new(app_magic8::Magic8AppFactory::new()),
            Box::new(app_sonos::SonosAppFactory::new()),
        ];
        Self::new_with_factories(factories)
    }

    pub fn new_with_factories(factories: Vec<Box<dyn AppFactory>>) -> Rc<RefCell<Self>> {
        let manager = Rc::new(RefCell::new(Self {
            launcher: None,
            factories,
            current_index: 0,
            active_app: None,
        }));
        Self::show_launcher(manager.clone());
        manager
    }

    pub fn show_launcher(manager: Rc<RefCell<Self>>) {
        manager.borrow_mut().active_app = None;

        let launcher = LauncherApp::new().expect("Failed to create LauncherApp");
        let current_index = manager.borrow().current_index;
        launcher.set_selected(current_index as i32);

        let cards: Vec<AppInfo> = manager
            .borrow()
            .factories
            .iter()
            .map(|f| f.info())
            .collect();
        launcher.set_cards(Rc::new(slint::VecModel::from(cards)).into());

        let mgr = manager.clone();
        launcher.on_launch(move |idx| {
            mgr.borrow_mut().launch_app(idx as usize, mgr.clone());
        });

        let _ = launcher.show();

        manager.borrow_mut().launcher = Some(launcher);
    }

    pub fn launch_app(&mut self, index: usize, manager: Rc<RefCell<Self>>) {
        if index >= self.factories.len() {
            return;
        }

        self.current_index = index;

        if let Some(ref l) = self.launcher {
            let _ = l.hide();
        }
        self.launcher = None;

        let mgr = manager.clone();
        let on_exit: Box<dyn Fn() + 'static> = Box::new(move || {
            Self::show_launcher(mgr.clone());
        });

        let app_instance = self.factories[index].launch(on_exit);
        self.active_app = Some(app_instance);
    }

    pub fn launcher(&self) -> Option<&LauncherApp> {
        self.launcher.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::Model;

    struct MockAppFactory(&'static str);

    impl AppFactory for MockAppFactory {
        fn info(&self) -> AppInfo {
            AppInfo {
                name: self.0.into(),
                accent: slint::Color::from_argb_u8(255, 255, 0, 0),
                icon: slint::Image::default(),
            }
        }

        fn launch(&self, _on_exit: Box<dyn Fn() + 'static>) -> Box<dyn Any> {
            Box::new(())
        }
    }

    fn test_factories() -> Vec<Box<dyn AppFactory>> {
        alloc::vec![
            Box::new(MockAppFactory("App 1")),
            Box::new(MockAppFactory("App 2")),
            Box::new(MockAppFactory("App 3")),
        ]
    }

    #[test]
    fn test_launcher() {
        if std::env::var("SLINT_BACKEND").is_err() {
            unsafe {
                std::env::set_var("SLINT_BACKEND", "headless");
            }
        }

        let launcher = LauncherApp::new().expect("Failed to create LauncherApp");
        let cards: Vec<AppInfo> = test_factories().iter().map(|f| f.info()).collect();
        launcher.set_cards(Rc::new(slint::VecModel::from(cards)).into());
        assert_eq!(launcher.get_cards().row_count(), 3);
        drop(launcher);

        let manager = LauncherManager::new_with_factories(test_factories());
        assert!(manager.borrow().launcher().is_some());
        assert_eq!(manager.borrow().current_index, 0);

        // Launch first app
        manager.borrow_mut().launch_app(0, manager.clone());
        assert!(manager.borrow().launcher().is_none());
        assert!(manager.borrow().active_app.is_some());

        // Return to launcher
        LauncherManager::show_launcher(manager.clone());
        assert!(manager.borrow().launcher().is_some());
        assert!(manager.borrow().active_app.is_none());
        assert_eq!(manager.borrow().current_index, 0);
    }
}
