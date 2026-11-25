//! The system power watcher behind the unlock grant (#10524).
//!
//! Why: AC3 — the grant must drop before the machine sleeps. A watcher that
//! cannot start means no grant; one that stops drops the grant (fail closed).
//! What: [`PowerSource`] is the seam: `subscribe` delivers [`PowerEvent`]s
//! to a hook until its [`Subscription`] drops, and `alive` says the source
//! still runs. [`OsPower`] on macOS is one process-wide IOKit thread
//! (`IORegisterForSystemPower` on its own `CFRunLoop`): on
//! `kIOMessageSystemWillSleep` it runs every hook, then lets the sleep go
//! ahead. Elsewhere it refuses to subscribe, so no grant opens. A missed
//! notification is still caught after wake by the grant's clock-gap check
//! (`store::grant_cache::SLEEP_GAP`).
//! Test: `unlock_tests.rs` drives a fake source;
//! `os_power_source_starts_on_macos` starts the real one, which shows no
//! prompt.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use thiserror::Error;

/// A power or session event that ends a grant.
#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "fired by the macOS watcher")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PowerEvent {
    /// The system is about to sleep.
    Sleep,
}

/// Called on each event, from the source's thread.
pub(crate) type PowerHook = Arc<dyn Fn(PowerEvent) + Send + Sync>;

/// The watcher could not start.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
#[error("the system power watcher is unavailable")]
pub(crate) struct WatchError;

/// Unsubscribes its hook when dropped.
pub(crate) struct Subscription(Option<Box<dyn FnOnce() + Send>>);

impl Subscription {
    /// A subscription that runs `cancel` when dropped.
    pub(crate) fn new(cancel: impl FnOnce() + Send + 'static) -> Self {
        Self(Some(Box::new(cancel)))
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(cancel) = self.0.take() {
            cancel();
        }
    }
}

/// Where power events come from.
pub(crate) trait PowerSource: Send + Sync {
    /// Deliver events to `hook` until the returned subscription drops.
    fn subscribe(&self, hook: PowerHook) -> Result<Subscription, WatchError>;
    /// Whether the source still runs; `false` once it has stopped.
    fn alive(&self) -> bool;
}

/// The hooks one source fans out to.
#[derive(Default)]
pub(crate) struct Hooks {
    next: AtomicU64,
    hooks: Mutex<Vec<(u64, PowerHook)>>,
}

impl Hooks {
    /// Add `hook`; the subscription removes it.
    pub(crate) fn add(self: &Arc<Self>, hook: PowerHook) -> Result<Subscription, WatchError> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        self.hooks.lock().map_err(|_| WatchError)?.push((id, hook));
        let hooks = Arc::clone(self);
        Ok(Subscription::new(move || {
            if let Ok(mut list) = hooks.hooks.lock() {
                list.retain(|(held, _)| *held != id);
            }
        }))
    }

    #[cfg_attr(
        not(target_os = "macos"),
        allow(dead_code, reason = "macOS watcher and tests")
    )]
    /// Run every hook with `event`. A poisoned list runs none; the source
    /// then reports itself dead, so every grant drops.
    pub(crate) fn fire(&self, event: PowerEvent) -> bool {
        let Ok(list) = self.hooks.lock() else {
            return false;
        };
        let hooks: Vec<PowerHook> = list.iter().map(|(_, hook)| Arc::clone(hook)).collect();
        drop(list);
        hooks.iter().for_each(|hook| hook(event));
        true
    }
}

/// The OS power source.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct OsPower;

/// The process-wide watcher: its hooks and whether its thread runs.
#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "built on macOS only")
)]
struct Watcher {
    hooks: Arc<Hooks>,
    alive: Arc<AtomicBool>,
}

static WATCHER: OnceLock<Option<Watcher>> = OnceLock::new();

impl PowerSource for OsPower {
    fn subscribe(&self, hook: PowerHook) -> Result<Subscription, WatchError> {
        let watcher = WATCHER.get_or_init(platform::start).as_ref();
        let watcher = watcher.filter(|w| w.alive.load(Ordering::SeqCst));
        watcher.ok_or(WatchError)?.hooks.add(hook)
    }

    fn alive(&self) -> bool {
        WATCHER
            .get()
            .and_then(Option::as_ref)
            .is_some_and(|w| w.alive.load(Ordering::SeqCst))
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    /// No power watcher off macOS, so no grant opens (fail closed).
    pub(super) fn start() -> Option<super::Watcher> {
        None
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    use super::{PowerEvent, WATCHER, Watcher};

    type IoObject = u32;
    type Callback = extern "C" fn(*mut c_void, IoObject, u32, *mut c_void);

    // IOKit/IOMessage.h: iokit_common_msg(0x270) and (0x280).
    const CAN_SYSTEM_SLEEP: u32 = 0xE000_0270;
    const SYSTEM_WILL_SLEEP: u32 = 0xE000_0280;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IORegisterForSystemPower(
            refcon: *mut c_void,
            port: *mut *mut c_void,
            callback: Callback,
            notifier: *mut IoObject,
        ) -> u32;
        fn IONotificationPortGetRunLoopSource(port: *mut c_void) -> *mut c_void;
        fn IOAllowPowerChange(kernel_port: u32, notification_id: isize) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFRunLoopDefaultMode: *const c_void;
        fn CFRunLoopGetCurrent() -> *mut c_void;
        fn CFRunLoopAddSource(run_loop: *mut c_void, source: *mut c_void, mode: *const c_void);
        fn CFRunLoopRun();
    }

    /// The root power port `IOAllowPowerChange` answers on.
    static ROOT_PORT: AtomicU32 = AtomicU32::new(0);

    /// The IOKit callback: drop every grant, then let the sleep proceed.
    extern "C" fn on_power(_: *mut c_void, _: IoObject, message: u32, argument: *mut c_void) {
        if message == SYSTEM_WILL_SLEEP
            && let Some(Some(watcher)) = WATCHER.get()
            && !watcher.hooks.fire(PowerEvent::Sleep)
        {
            // #10524: hooks that could not run leave no grant behind.
            watcher.alive.store(false, Ordering::SeqCst);
        }
        if message == SYSTEM_WILL_SLEEP || message == CAN_SYSTEM_SLEEP {
            // SAFETY: the port came from `IORegisterForSystemPower`; the id
            // is the one IOKit passed for this message.
            unsafe { IOAllowPowerChange(ROOT_PORT.load(Ordering::SeqCst), argument as isize) };
        }
    }

    /// Start the watcher thread; `None` when it cannot register.
    pub(super) fn start() -> Option<Watcher> {
        let (ready, registered) = mpsc::channel();
        let alive = Arc::new(AtomicBool::new(true));
        let thread_alive = Arc::clone(&alive);
        std::thread::Builder::new()
            .name("trusty-secrets-power".to_owned())
            .spawn(move || {
                let (mut port, mut notifier) = (std::ptr::null_mut(), 0);
                // SAFETY: out-pointers to locals; the callback is `extern "C"`.
                let root = unsafe {
                    IORegisterForSystemPower(
                        std::ptr::null_mut(),
                        &mut port,
                        on_power,
                        &mut notifier,
                    )
                };
                if root == 0 || port.is_null() {
                    let _ = ready.send(false);
                    return;
                }
                ROOT_PORT.store(root, Ordering::SeqCst);
                // SAFETY: `port` is the live notification port from above.
                unsafe {
                    let source = IONotificationPortGetRunLoopSource(port);
                    CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopDefaultMode);
                }
                let _ = ready.send(true);
                // SAFETY: runs this thread's run loop; returns only if stopped.
                unsafe { CFRunLoopRun() };
                // #10524: a watcher that stops drops every grant.
                thread_alive.store(false, Ordering::SeqCst);
            })
            .ok()?;
        match registered.recv_timeout(Duration::from_secs(5)) {
            Ok(true) => Some(Watcher {
                hooks: Arc::default(),
                alive,
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: #10524 — the real macOS source registers without a prompt and
    /// reports itself alive; elsewhere it refuses, so no grant opens.
    /// Test: itself.
    #[test]
    fn os_power_source_starts_on_macos() {
        let subscribed = OsPower.subscribe(Arc::new(|_| {}));
        assert_eq!(subscribed.is_ok(), cfg!(target_os = "macos"));
        assert_eq!(OsPower.alive(), cfg!(target_os = "macos"));
    }
}
