/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use std::ffi::CString;

pub(crate) const DEFAULT_LOCALE: &str = "en-US";
pub(crate) const DEFAULT_TIMEZONE: &str = "UTC";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderEnvironment {
    pub locale: &'static str,
    pub timezone: &'static str,
}

impl Default for RenderEnvironment {
    fn default() -> Self {
        Self {
            locale: DEFAULT_LOCALE,
            timezone: DEFAULT_TIMEZONE,
        }
    }
}

impl RenderEnvironment {
    pub(crate) fn artifact(self) -> serde_json::Value {
        serde_json::json!({
            "locale": {
                "requested": self.locale,
                "resolved": self.locale,
            },
            "timezone": {
                "requested": self.timezone,
                "resolved": self.timezone,
            },
        })
    }
}

#[cfg(all(unix, not(any(target_os = "android", target_env = "ohos"))))]
#[allow(unsafe_code)]
unsafe extern "C" {
    fn tzset();
}

/// Sets the process-global timezone before Servo starts any worker threads.
/// This is deliberately scoped to Pliego's one-render-per-process CLI model.
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
pub(crate) fn apply_timezone(timezone: &str) -> Result<(), String> {
    let variable = CString::new("TZ").map_err(|error| error.to_string())?;
    let value = CString::new(timezone).map_err(|_| "timezone contains a null byte")?;

    #[cfg(target_os = "windows")]
    let result = unsafe { libc::putenv_s(variable.as_ptr(), value.as_ptr()) };
    #[cfg(unix)]
    let result = unsafe { libc::setenv(variable.as_ptr(), value.as_ptr(), 1) };
    #[cfg(not(any(target_os = "windows", unix)))]
    return Err("timezone overrides are unsupported on this desktop target".into());

    #[cfg(any(target_os = "windows", unix))]
    {
        if result != 0 {
            return Err(format!(
                "cannot set process timezone to {timezone}: platform error {result}"
            ));
        }

        // Keep the C runtime and SpiderMonkey's later cache reset on the same value.
        #[cfg(target_os = "windows")]
        unsafe {
            libc::tzset()
        };
        #[cfg(unix)]
        unsafe {
            tzset()
        };
        Ok(())
    }
}
