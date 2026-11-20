//! The device's locale, read from the platform where it isn't in the
//! process environment: Android apps get no `LANG`/`LC_*` variables, so
//! there it comes from `java.util.Locale.getDefault()` over JNI.
//! Elsewhere there's nothing to read here, and callers fall back to the
//! environment.

/// The device's language as the platform reports it (e.g. `"fr"`), or
/// `None` where it isn't read from the platform (every target but Android)
/// or couldn't be read (logged).
pub(crate) fn device_language() -> Option<String> {
    #[cfg(target_os = "android")]
    {
        android::language()
            .inspect_err(|err| tracing::warn!("failed to read the device's locale: {err}"))
            .ok()
    }
    #[cfg(not(target_os = "android"))]
    {
        None
    }
}

#[cfg(target_os = "android")]
mod android {
    use jni::JavaVM;
    use jni::objects::JString;

    /// `Locale.getDefault().getLanguage()`, through the `JavaVM` the app
    /// registers at startup with `Keyring.initializeNdkContext` (the
    /// keyring store needs it too: see the sdk CLAUDE.md's `secrets`).
    pub(super) fn language() -> Result<String, String> {
        // `android_context` panics when the app never registered one.
        let context = std::panic::catch_unwind(ndk_context::android_context)
            .map_err(|_| "the Android context was never initialized".to_string())?;
        // SAFETY: `ndk_context` only ever holds the process' `JavaVM`.
        let vm = unsafe { JavaVM::from_raw(context.vm().cast()) }
            .map_err(|err| format!("invalid JavaVM: {err}"))?;
        // A no-op on a thread the JVM already knows (a Kotlin caller's);
        // otherwise attached until `env` drops.
        let mut env = vm
            .attach_current_thread()
            .map_err(|err| format!("failed to attach to the JVM: {err}"))?;

        let language = (|| {
            let locale = env
                .call_static_method(
                    "java/util/Locale",
                    "getDefault",
                    "()Ljava/util/Locale;",
                    &[],
                )?
                .l()?;
            let language = env
                .call_method(&locale, "getLanguage", "()Ljava/lang/String;", &[])?
                .l()?;
            let language: String = env.get_string(&JString::from(language))?.into();
            Ok::<_, jni::errors::Error>(language)
        })();
        language.map_err(|err| {
            // A thrown exception stays pending until cleared, and would
            // break the thread's next JNI call.
            let _ = env.exception_clear();
            format!("failed to call java.util.Locale: {err}")
        })
    }
}
