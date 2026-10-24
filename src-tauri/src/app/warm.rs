//! Estuary: message windows without a blank phase.
//!
//! A new WKWebView window shows nothing until its page has loaded and painted. Two layers:
//! - Prewarm: one hidden window (label `pake-warm-N`) has already loaded the Estuary SPA at
//!   `/m/__prewarm`. `open_window` for a message hands the message to it in-page
//!   (`window.__estuaryOpenMessage(path)`, client-side routing, no reload), waits for the page's
//!   `window_content_ready` (max 800 ms), then shows it. A fresh prewarm window follows.
//!   The page reports `prewarm_ready` when it supports this; a page that never does (older web
//!   build) is closed after a grace period and prewarming stays off for this run.
//! - Fallback: no prewarmed window ready -> the window is built hidden and shown on
//!   `window_content_ready`, the first finished page load, or after 1.5 s, whichever is first.
//!
//! Timings go to ~/Library/Logs/Estuary/window-timing.log.

use std::collections::HashMap;
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager, WebviewWindow};
use tokio::sync::oneshot;

pub const WARM_PREFIX: &str = "pake-warm-";
pub const PREWARM_PATH: &str = "/m/__prewarm";
const PREWARM_GRACE: Duration = Duration::from_secs(20);

#[derive(Default)]
struct Inner {
    creating: Option<String>,
    ready: Option<String>,
    waiters: HashMap<String, oneshot::Sender<()>>,
    opened_at: HashMap<String, Instant>,
    next: u32,
    background: Option<(u8, u8, u8)>,
    started: bool,
    disabled: bool,
}

fn inner() -> &'static Mutex<Inner> {
    static S: OnceLock<Mutex<Inner>> = OnceLock::new();
    S.get_or_init(Default::default)
}

fn start() -> Instant {
    static T: OnceLock<Instant> = OnceLock::new();
    *T.get_or_init(Instant::now)
}

/// Append one timing line (ms since app start + wall clock ms).
pub fn tlog(msg: impl AsRef<str>) {
    let since = start().elapsed().as_millis();
    let wall = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let line = format!("{wall} +{since}ms {}\n", msg.as_ref());
    eprint!("[estuary-window] {line}");
    if let Some(home) = std::env::var_os("HOME") {
        let dir = std::path::Path::new(&home).join("Library/Logs/Estuary");
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("window-timing.log"))
        {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

pub fn init_clock() {
    let _ = start();
}

/// A hidden prewarm window (not yet handed a message): never revealed by generic code paths.
pub fn is_hidden_warm(label: &str) -> bool {
    let s = inner().lock().unwrap();
    s.creating.as_deref() == Some(label) || s.ready.as_deref() == Some(label)
}

pub fn set_background(rgb: (u8, u8, u8)) {
    inner().lock().unwrap().background = Some(rgb);
}

pub fn background() -> Option<tauri::window::Color> {
    inner()
        .lock()
        .unwrap()
        .background
        .map(|(r, g, b)| tauri::window::Color(r, g, b, 255))
}

/// Main window finished its first load: start prewarming once, after it has settled.
pub fn on_main_loaded(app: &AppHandle) {
    if !prewarm_enabled() {
        return;
    }
    {
        let mut s = inner().lock().unwrap();
        if s.started {
            return;
        }
        s.started = true;
    }
    schedule_prewarm(app, Duration::from_secs(4));
}

pub fn schedule_prewarm(app: &AppHandle, delay: Duration) {
    if !prewarm_enabled() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(delay).await;
        create_prewarm(&app);
    });
}

fn create_prewarm(app: &AppHandle) {
    let label = {
        let mut s = inner().lock().unwrap();
        if s.disabled || s.creating.is_some() || s.ready.is_some() {
            return;
        }
        s.next += 1;
        let label = format!("{WARM_PREFIX}{}", s.next);
        s.creating = Some(label.clone());
        label
    };
    let Some(url) = app
        .get_webview_window("pake")
        .and_then(|w| w.url().ok())
        .and_then(|u| u.join(PREWARM_PATH).ok())
    else {
        inner().lock().unwrap().creating = None;
        return;
    };
    let t = Instant::now();
    match crate::app::window::build_prewarm_window(app, &label, url, background()) {
        Ok(window) => {
            set_in_windows_menu(&window, false);
            tlog(format!("{label} prewarm built in {}ms", t.elapsed().as_millis()));
            // An older web build never reports prewarm_ready: drop the window, stop prewarming.
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(PREWARM_GRACE).await;
                let stale = {
                    let mut s = inner().lock().unwrap();
                    if s.creating.as_deref() == Some(label.as_str()) {
                        s.creating = None;
                        s.disabled = true;
                        true
                    } else {
                        false
                    }
                };
                if stale {
                    tlog(format!("{label} never reported prewarm_ready; prewarming off"));
                    if let Some(w) = app.get_webview_window(&label) {
                        let _ = w.destroy();
                    }
                }
            });
        }
        Err(e) => {
            inner().lock().unwrap().creating = None;
            tlog(format!("{label} prewarm build failed: {e}"));
        }
    }
}

/// Page in a prewarm window is up and can take a message.
pub fn mark_ready(window: &WebviewWindow) {
    let label = window.label().to_string();
    let mut s = inner().lock().unwrap();
    if s.creating.as_deref() == Some(label.as_str()) {
        s.creating = None;
        s.ready = Some(label.clone());
        drop(s);
        tlog(format!("{label} prewarm_ready"));
    }
}

pub fn take_ready(app: &AppHandle) -> Option<WebviewWindow> {
    let label = inner().lock().unwrap().ready.take()?;
    app.get_webview_window(&label)
}

/// Resolves when the page in `label` reports `window_content_ready`.
pub fn wait_for_content(label: &str) -> oneshot::Receiver<()> {
    let (tx, rx) = oneshot::channel();
    let mut s = inner().lock().unwrap();
    s.waiters.insert(label.to_string(), tx);
    s.opened_at.insert(label.to_string(), Instant::now());
    rx
}

pub fn content_ready(window: &WebviewWindow) {
    let label = window.label().to_string();
    let (tx, at) = {
        let mut s = inner().lock().unwrap();
        (s.waiters.remove(&label), s.opened_at.remove(&label))
    };
    let ms = at.map(|t| t.elapsed().as_millis() as i64).unwrap_or(-1);
    tlog(format!("{label} content_ready +{ms}ms after open"));
    if let Some(tx) = tx {
        let _ = tx.send(());
    }
}

pub fn ms_since_open(label: &str) -> i64 {
    inner()
        .lock()
        .unwrap()
        .opened_at
        .get(label)
        .map(|t| t.elapsed().as_millis() as i64)
        .unwrap_or(-1)
}

/// Show + focus a hidden window (no-op when already visible).
pub fn reveal(window: &WebviewWindow, why: &str, since: Instant) {
    if window.is_visible().unwrap_or(true) {
        return;
    }
    set_in_windows_menu(window, true);
    let _ = window.show();
    crate::app::window::reapply_window_icon(window);
    let _ = window.set_focus();
    tlog(format!(
        "{} shown ({why}) {}ms after open_window",
        window.label(),
        since.elapsed().as_millis()
    ));
}

/// Hidden prewarm windows stay out of the Window menu (macOS).
pub fn set_in_windows_menu(window: &WebviewWindow, include: bool) {
    #[cfg(target_os = "macos")]
    {
        let w = window.clone();
        let _ = window.run_on_main_thread(move || {
            if let Ok(ptr) = w.ns_window() {
                unsafe {
                    if let Some(ns) = objc2::rc::Retained::retain(ptr as *mut objc2_app_kit::NSWindow) {
                        ns.setExcludedFromWindowsMenu(!include);
                    }
                }
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (window, include);
}

/// True while open_window is waiting for this window's content signal.
pub fn is_waiting(label: &str) -> bool {
    inner().lock().map(|s| s.waiters.contains_key(label)).unwrap_or(false)
}

/// Always-on prewarm costs a whole WebContent process; off unless ESTUARY_PREWARM=1.
pub fn prewarm_enabled() -> bool {
    std::env::var("ESTUARY_PREWARM").map(|v| v == "1").unwrap_or(false)
}
