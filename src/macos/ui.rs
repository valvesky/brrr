//! AppKit owns the main thread. The engine sends owned Rust state through
//! the main dispatch queue; Objective-C objects never cross that boundary.

use super::{power, processes, setup};
use crate::core::{
    config::{self, Edit},
    engine::{Event, Events, Profile, State},
};
use dispatch2::DispatchQueue;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::Sel;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSApplicationActivationPolicy,
    NSControlStateValueOff, NSControlStateValueOn, NSEventModifierFlags, NSImage, NSMenu,
    NSMenuItem, NSStatusBar, NSStatusItem, NSTextField, NSVariableStatusItemLength, NSWorkspace,
};
use objc2_foundation::{
    MainThreadMarker, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSURL,
};
use std::cell::{Cell, RefCell};
use std::sync::mpsc::Receiver;

thread_local! {
    /// Retain the action target for the full lifetime of every menu item.
    static TARGET: RefCell<Option<Retained<MenuTarget>>> = const { RefCell::new(None) };
}

struct Ivars {
    state: RefCell<State>,
    status: Retained<NSStatusItem>,
    events: Events,
    power: power::Control,
    busy: Cell<bool>,
}

define_class!(
    // SAFETY: NSObject permits subclassing and all AppKit access is main-thread-only.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "brrrMenuTarget"]
    #[ivars = Ivars]
    struct MenuTarget;

    // SAFETY: NSObjectProtocol introduces no additional invariants.
    unsafe impl NSObjectProtocol for MenuTarget {}

    impl MenuTarget {
        // SAFETY: Each action has AppKit's void-returning, one-object signature.
        #[unsafe(method(toggleAutomatic:))]
        fn toggle_automatic(&self, _sender: &NSMenuItem) {
            let state = self.ivars().state.borrow().clone();
            if state.config_error.is_none() {
                self.edit(Edit::Enable(!state.enabled));
            }
        }

        #[unsafe(method(removeCommand:))]
        fn remove_command(&self, sender: &NSMenuItem) {
            let command = sender.title().to_string();
            let alert = alert(self.mtm(), "Stop watching this app", &format!("Stop watching “{command}”?"));
            alert.addButtonWithTitle(&NSString::from_str("Remove"));
            alert.addButtonWithTitle(&NSString::from_str("Cancel"));
            activate(self.mtm());
            if alert.runModal() == NSAlertFirstButtonReturn {
                self.edit(Edit::Remove(command));
            }
        }

        #[unsafe(method(addCommand:))]
        fn add_command(&self, _sender: &NSMenuItem) {
            let mtm = self.mtm();
            let alert = alert(mtm, "Add app", "Enter the process name to watch, such as ffmpeg or HandBrake.");
            let field = NSTextField::initWithFrame(NSTextField::alloc(mtm), NSRect::new(
                NSPoint::new(0.0, 0.0), NSSize::new(300.0, 24.0),
            ));
            field.setPlaceholderString(Some(&NSString::from_str("Command name")));
            alert.setAccessoryView(Some(&field));
            alert.addButtonWithTitle(&NSString::from_str("OK"));
            alert.addButtonWithTitle(&NSString::from_str("Cancel"));
            activate(mtm);
            alert.layout();
            alert.window().setInitialFirstResponder(Some(&field));
            alert.window().makeFirstResponder(Some(&field));
            if alert.runModal() == NSAlertFirstButtonReturn {
                let command = field.stringValue().to_string();
                if config::valid_name(&command) {
                    self.edit(Edit::Add(command));
                } else {
                    error(mtm, &format!("A command name must be 1–{} bytes without slashes, newlines or NULs.", config::MAX_NAME_BYTES));
                }
            }
        }

        #[unsafe(method(openConfiguration:))]
        fn open_configuration(&self, _sender: &NSMenuItem) {
            let enabled = self.ivars().state.borrow().enabled;
            self.work(move || {
                let path = config::config_path();
                if !path.exists() { config::edit(&Edit::Enable(enabled))?; }
                DispatchQueue::main().exec_async(move || {
                    autoreleasepool(|_| {
                        let mtm = MainThreadMarker::new().expect("dispatch main queue");
                        match NSURL::from_path(&path, false, None) {
                            Some(url) if NSWorkspace::sharedWorkspace().openURL(&url) => {}
                            _ => error(mtm, "Could not open the configuration file."),
                        }
                    });
                });
                Ok(())
            });
        }

        #[unsafe(method(toggleLogin:))]
        fn toggle_login(&self, _sender: &NSMenuItem) {
            self.work(|| setup::set_login(!setup::login_enabled()));
        }

        #[unsafe(method(allowPowerSwitching:))]
        fn allow_power_switching(&self, _sender: &NSMenuItem) {
            activate(self.mtm());
            let control = self.ivars().power.clone();
            self.work(move || {
                setup::allow_power_switching()?;
                control.recheck();
                Ok(())
            });
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: &NSMenuItem) {
            let _ = self.ivars().events.send(Event::Stop);
        }
    }
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker, events: Events, power: power::Control) -> Retained<Self> {
        let status =
            NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        let this = Self::alloc(mtm).set_ivars(Ivars {
            state: RefCell::new(State::default()),
            status,
            events,
            power,
            busy: Cell::new(false),
        });
        // SAFETY: NSObject init has this signature and no further invariants.
        unsafe { msg_send![super(this), init] }
    }

    fn edit(&self, edit: Edit) {
        self.work(move || config::edit(&edit));
    }

    fn work(&self, work: impl FnOnce() -> Result<(), String> + Send + 'static) {
        if self.ivars().busy.replace(true) {
            return;
        }
        self.render();
        let result = std::thread::Builder::new()
            .name("brrr-menu".into())
            .spawn(move || {
                let result = work();
                DispatchQueue::main().exec_async(move || {
                    autoreleasepool(|_| {
                        with_target(|target| {
                            target.ivars().busy.set(false);
                            target.render();
                            if let Err(message) = result {
                                error(target.mtm(), &message);
                            }
                        })
                    });
                });
            });
        if let Err(message) = result {
            self.ivars().busy.set(false);
            self.render();
            error(
                self.mtm(),
                &format!("Could not start menu operation: {message}"),
            );
        }
    }

    fn item(
        &self,
        menu: &NSMenu,
        title: &str,
        action: Option<Sel>,
        checked: bool,
        enabled: bool,
    ) -> Retained<NSMenuItem> {
        // SAFETY: All actions passed here are selectors implemented above with
        // AppKit's action signature. The TLS reference keeps their target alive.
        let item = unsafe {
            let item = NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(self.mtm()),
                &NSString::from_str(title),
                action,
                &NSString::from_str(if action == Some(sel!(quit:)) { "q" } else { "" }),
            );
            if action.is_some() {
                item.setTarget(Some(self));
            }
            item
        };
        item.setState(if checked {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
        item.setEnabled(enabled);
        item.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
        menu.addItem(&item);
        item
    }

    fn render(&self) {
        let state = self.ivars().state.borrow().clone();
        let mtm = self.mtm();
        let summary = NSString::from_str(&state.summary());
        if let Some(button) = self.ivars().status.button(mtm) {
            let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                &NSString::from_str(symbol(state.profile)),
                Some(&NSString::from_str("brrr")),
            );
            if let Some(image) = &image {
                image.setTemplate(true);
            }
            button.setImage(image.as_deref());
            button.setToolTip(Some(&summary));
        }
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);
        let ready = !self.ivars().busy.get();
        self.item(&menu, &state.summary(), None, false, false);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        self.item(
            &menu,
            "Automatic Boost",
            Some(sel!(toggleAutomatic:)),
            state.enabled,
            ready && state.config_error.is_none(),
        );
        let watched = self.item(&menu, "Watched apps", None, false, true);
        let commands = NSMenu::new(mtm);
        commands.setAutoenablesItems(false);
        for command in &state.commands {
            self.item(
                &commands,
                command,
                Some(sel!(removeCommand:)),
                state.active.contains(command),
                ready,
            );
        }
        if !state.commands.is_empty() {
            commands.addItem(&NSMenuItem::separatorItem(mtm));
        }
        self.item(
            &commands,
            "Add app…",
            Some(sel!(addCommand:)),
            false,
            ready,
        );
        watched.setSubmenu(Some(&commands));
        self.item(
            &menu,
            "Open Configuration File",
            Some(sel!(openConfiguration:)),
            false,
            ready,
        );
        self.item(
            &menu,
            "Start at Login",
            Some(sel!(toggleLogin:)),
            setup::login_enabled(),
            ready,
        );
        if !state.available {
            self.item(
                &menu,
                "Allow Power Mode Switching…",
                Some(sel!(allowPowerSwitching:)),
                false,
                ready,
            );
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        self.item(&menu, "Quit", Some(sel!(quit:)), false, true);
        self.ivars().status.setMenu(Some(&menu));
    }
}

/// Clone before calling AppKit: modal alerts may reenter the main queue.
/// No RefCell borrow is held across a native callback or nested run loop.
fn with_target(work: impl FnOnce(&MenuTarget)) {
    let target = TARGET.with(|target| target.borrow().clone());
    if let Some(target) = target {
        work(&target);
    }
}

fn symbol(profile: Profile) -> &'static str {
    match profile {
        Profile::Performance => "bolt.fill",
        Profile::Balanced => "circle.lefthalf.filled",
        Profile::PowerSaver => "leaf.fill",
        Profile::Unknown => "bolt",
    }
}

fn activate(mtm: MainThreadMarker) {
    // This older API also brings an unbundled, accessory application forward
    // on macOS versions predating NSApplication.activate (macOS 14).
    #[allow(deprecated)]
    NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
}

fn alert(mtm: MainThreadMarker, title: &str, message: &str) -> Retained<NSAlert> {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(message));
    alert
}

fn error(mtm: MainThreadMarker, message: &str) {
    let alert = alert(mtm, "brrr", message);
    alert.addButtonWithTitle(&NSString::from_str("OK"));
    activate(mtm);
    alert.runModal();
}

fn remove_icon() {
    let target = TARGET.with(|target| target.borrow_mut().take());
    if let Some(target) = target {
        target.ivars().status.setMenu(None);
        NSStatusBar::systemStatusBar().removeStatusItem(&target.ivars().status);
    }
}

pub fn run(
    events: Events,
    receiver: Receiver<Event>,
    power: power::Power,
    processes: processes::Processes,
) -> i32 {
    let Some(mtm) = MainThreadMarker::new() else {
        return setup::report(Err("AppKit must run on the main thread".into()));
    };
    autoreleasepool(|_| {
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        let target = MenuTarget::new(mtm, events.clone(), power.control());
        TARGET.with(|slot| *slot.borrow_mut() = Some(target.clone()));
        target.render();
        let engine_events = events.clone();
        let engine = std::thread::Builder::new()
            .name("brrr-engine".into())
            .spawn(move || {
                let frontend = Box::new(|state: &State| {
                    let state = state.clone();
                    DispatchQueue::main().exec_async(move || {
                        autoreleasepool(|_| {
                            with_target(|target| {
                                *target.ivars().state.borrow_mut() = state;
                                target.render();
                            })
                        });
                    });
                    true
                });
                let status = super::run_engine(engine_events, receiver, power, processes, frontend);
                DispatchQueue::main().exec_async(move || {
                    remove_icon();
                    // The engine has synchronously restored its power changes.
                    std::process::exit(status);
                });
                status
            });
        let engine = match engine {
            Ok(engine) => engine,
            Err(error) => {
                remove_icon();
                return setup::report(Err(format!("start engine: {error}")));
            }
        };
        app.run();
        // Defensive cleanup if AppKit's run loop is stopped by the system.
        let _ = events.send(Event::Stop);
        let result = engine.join().unwrap_or(1);
        remove_icon();
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_symbols_cover_all_profiles() {
        assert_eq!(symbol(Profile::Performance), "bolt.fill");
        assert_eq!(symbol(Profile::Balanced), "circle.lefthalf.filled");
        assert_eq!(symbol(Profile::PowerSaver), "leaf.fill");
        assert_eq!(symbol(Profile::Unknown), "bolt");
    }
}
