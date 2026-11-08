//! Launch at login, controlled by the remote page.
//!
//! macOS 13 and later register the running app with `SMAppService.mainApp`.
//! That tracks the bundle instead of a hardcoded path, and it works for the
//! ad-hoc signature this shell already uses (`signingIdentity: "-"`) when the
//! app lives in `/Applications`. `tauri-plugin-autostart` cannot select that
//! backend: it only installs a LaunchAgent plist or drives System Events via
//! AppleScript. A plist breaks as soon as the app moves and still reports
//! itself enabled, and AppleScript needs an Automation prompt.
//!
//! If `SMAppService` is unavailable (macOS older than 13, or it rejects the
//! signature), registration falls back to a per-user LaunchAgent whose
//! program is the current executable. Linux and Windows keep a native login
//! entry through the same helper so those builds still link; macOS is the
//! supported target.

use auto_launch::{AutoLaunch, AutoLaunchBuilder, Error, MacOSLaunchMode, WindowsEnableMode};
use tauri::{command, AppHandle};

/// `kSMErrorInvalidSignature`.
const SM_INVALID_SIGNATURE: u32 = 3;
/// `kSMErrorJobNotFound`. Unregistering an item that is already gone.
const SM_JOB_NOT_FOUND: u32 = 6;
/// `kSMErrorAlreadyRegistered`. Registering an item that is already present.
const SM_ALREADY_REGISTERED: u32 = 12;

#[derive(Debug, PartialEq, Eq)]
enum SmEnable {
    /// Registered, including the case where it was already registered.
    Ready,
    /// The signature cannot use `SMAppService`; use a LaunchAgent instead.
    Fallback,
    Failed(String),
}

fn classify_sm_enable(result: Result<(), Error>) -> SmEnable {
    match result {
        Ok(()) => SmEnable::Ready,
        Err(Error::SMAppServiceRegistrationFailed(SM_ALREADY_REGISTERED)) => SmEnable::Ready,
        Err(Error::SMAppServiceRegistrationFailed(SM_INVALID_SIGNATURE)) => SmEnable::Fallback,
        Err(error) => SmEnable::Failed(error.to_string()),
    }
}

fn classify_disable(result: Result<(), Error>) -> Result<(), String> {
    match result {
        Ok(()) => Ok(()),
        Err(Error::SMAppServiceUnregistrationFailed(SM_JOB_NOT_FOUND)) => Ok(()),
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn build_launch(name: &str, path: &str, mode: MacOSLaunchMode) -> Result<AutoLaunch, Error> {
    AutoLaunchBuilder::new()
        .set_app_name(name)
        .set_app_path(path)
        .set_macos_launch_mode(mode)
        .set_windows_enable_mode(WindowsEnableMode::CurrentUser)
        .build()
}

/// `None` on macOS older than 13, where `SMAppService` does not exist.
/// On every other platform the macOS mode is ignored and this is the native
/// login entry (XDG autostart or the current-user Run key).
fn service_manager(name: &str, path: &str) -> Result<Option<AutoLaunch>, String> {
    match build_launch(name, path, MacOSLaunchMode::SMAppService) {
        Ok(launch) => Ok(Some(launch)),
        Err(Error::UnsupportedOS) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn launch_agent(name: &str, path: &str) -> Result<AutoLaunch, String> {
    build_launch(name, path, MacOSLaunchMode::LaunchAgent).map_err(|error| error.to_string())
}

fn identity(app: &AppHandle) -> Result<(String, String), String> {
    let app_name = app.config().identifier.trim().to_string();
    if app_name.is_empty() || app_name.contains(['/', '\\', '\0']) {
        return Err(
            "autostart: the app identifier cannot be used as a login-item name".to_string(),
        );
    }
    let executable = std::env::current_exe().map_err(|error| format!("autostart: {error}"))?;
    if !executable.is_absolute() {
        return Err("autostart: the executable path is not absolute".to_string());
    }
    let app_path = executable
        .to_str()
        .filter(|path| !path.is_empty())
        .ok_or("autostart: the executable path is not valid UTF-8")?
        .to_string();
    Ok((app_name, app_path))
}

/// `true` only when the platform will actually launch the app.
///
/// On macOS 13+ that is `SMAppService` status `Enabled`. `RequiresApproval`
/// (registered, but not allowed to run until the user acts in System Settings)
/// is `false`, so a settings toggle does not claim the app will open at login.
/// A legacy LaunchAgent plist left behind by the fallback also counts.
fn registered(name: &str, path: &str) -> Result<bool, String> {
    if let Some(service) = service_manager(name, path)? {
        if service.is_enabled().map_err(|error| error.to_string())? {
            return Ok(true);
        }
    }
    // The non-macOS service manager is already the only login entry.
    if cfg!(target_os = "macos") {
        return launch_agent(name, path)?
            .is_enabled()
            .map_err(|error| error.to_string());
    }
    Ok(false)
}

fn enable(name: &str, path: &str) -> Result<(), String> {
    if let Some(service) = service_manager(name, path)? {
        match classify_sm_enable(service.enable()) {
            SmEnable::Ready => {
                // A previous fallback plist would start a second copy.
                if cfg!(target_os = "macos") {
                    classify_disable(launch_agent(name, path)?.disable())?;
                }
                return Ok(());
            }
            SmEnable::Fallback if cfg!(target_os = "macos") => {}
            SmEnable::Fallback => {
                return Err("autostart: SMAppService rejected the app signature".to_string());
            }
            SmEnable::Failed(message) => return Err(message),
        }
    }

    launch_agent(name, path)?
        .enable()
        .map_err(|error| error.to_string())
}

fn disable(name: &str, path: &str) -> Result<(), String> {
    if let Some(service) = service_manager(name, path)? {
        classify_disable(service.disable())?;
    }
    if cfg!(target_os = "macos") {
        classify_disable(launch_agent(name, path)?.disable())?;
    }
    Ok(())
}

fn on_main<T: Send + 'static>(
    app: &AppHandle,
    task: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    app.run_on_main_thread(move || {
        let _ = sender.send(task());
    })
    .map_err(|error| format!("autostart: {error}"))?;
    receiver
        .recv()
        .map_err(|_| "autostart: main thread dropped the result".to_string())
}

/// Whether this app is registered to launch at login.
///
/// Resolves to a boolean. `true` means the platform reports the login item as
/// enabled. On macOS 13 and later a registration that is still waiting for
/// approval in System Settings resolves to `false`.
#[command]
pub fn get_autostart(app: AppHandle) -> Result<bool, String> {
    let (name, path) = identity(&app)?;
    on_main(&app, move || registered(&name, &path))?
}

/// Register or remove the login item.
///
/// Argument: `{ enabled: boolean }`. Resolves to the boolean state after the
/// change, with the same meaning as [`get_autostart`]. Requesting the current
/// state again succeeds. A resolved `false` after `{ enabled: true }` means
/// macOS left the item waiting for approval; it is not a rejected call.
/// Rejections are strings.
#[command]
pub fn set_autostart(app: AppHandle, enabled: bool) -> Result<bool, String> {
    let (name, path) = identity(&app)?;
    on_main(&app, move || {
        if enabled {
            enable(&name, &path)?;
        } else {
            disable(&name, &path)?;
        }
        registered(&name, &path)
    })?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn already_registered_is_success_and_a_rejected_signature_falls_back() {
        assert_eq!(classify_sm_enable(Ok(())), SmEnable::Ready);
        assert_eq!(
            classify_sm_enable(Err(Error::SMAppServiceRegistrationFailed(
                SM_ALREADY_REGISTERED
            ))),
            SmEnable::Ready
        );
        assert_eq!(
            classify_sm_enable(Err(Error::SMAppServiceRegistrationFailed(
                SM_INVALID_SIGNATURE
            ))),
            SmEnable::Fallback
        );
        assert!(matches!(
            classify_sm_enable(Err(Error::SMAppServiceRegistrationFailed(2))),
            SmEnable::Failed(_)
        ));
    }

    #[test]
    fn disabling_a_missing_login_item_is_success() {
        assert!(classify_disable(Ok(())).is_ok());
        assert!(
            classify_disable(Err(Error::SMAppServiceUnregistrationFailed(
                SM_JOB_NOT_FOUND
            )))
            .is_ok()
        );
        assert!(classify_disable(Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "missing"
        ))))
        .is_ok());
        assert!(classify_disable(Err(Error::UnsupportedOS)).is_err());
    }

    #[test]
    fn querying_autostart_does_not_create_a_login_entry() {
        let name = "com.pake.estuary.autostart.query-test";
        let path = "/tmp/pake-estuary-autostart-does-not-exist";
        assert_eq!(registered(name, path).unwrap(), false);

        if let Ok(home) = std::env::var("HOME") {
            let desktop = std::path::Path::new(&home)
                .join(".config/autostart")
                .join(format!("{name}.desktop"));
            assert!(!desktop.exists(), "query created {desktop:?}");
            let plist = std::path::Path::new(&home)
                .join("Library/LaunchAgents")
                .join(format!("{name}.plist"));
            assert!(!plist.exists(), "query created {plist:?}");
        }
    }
}
