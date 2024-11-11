//! Android USB serial driver, currently works with CDC-ACM devices.
//!
//! It is far from being feature-complete. Of course you can make use of something like
//! [react-native-usb-serialport](https://www.npmjs.com/package/react-native-usb-serialport),
//! however, that may introduce multiple layers between Rust and the Linux kernel.
//!
//! This crate requires `ndk_context::AndroidContext`, usually initialized by
//! crate `android_activity`. `jni::JavaVM::attach_current_thread_permanently()` is called.
//!
//! The older version of this crate performs USB transfers through JNI calls but not `nusb`,
//! do not use it except you have encountered compatibility problems.

mod ser_cdc;
mod usb_conn;
mod usb_info;
mod usb_sync;
pub use ser_cdc::*;

/// Equals `std::io::Error`.
pub type Error = std::io::Error;

/// Android helper for `nusb`. It needs enhancements before merging into that crate.
///
/// Reference:
/// - <https://developer.android.com/develop/connectivity/usb/host>
/// - <https://developer.android.com/reference/android/hardware/usb/package-summary>
pub mod usb {
    pub use crate::usb_conn::*;
    pub use crate::usb_info::*;
    pub use crate::usb_sync::*;
    pub use crate::Error;

    /// Different threads must use different `JNIEnv` while sharing the same JVM.
    /// `jni::JavaVM::attach_current_thread()` allows nested (multiple) calls.
    /// Note: it assumes the current context is initialized by `android-activity`,
    /// then `with_jni_env_ctx()` calls throughout the code actually uses handlers
    /// of the same Android context (the handler `AndroidContext` is `Copy`).
    #[inline(always)]
    pub(crate) fn with_jni_env_ctx<R>(
        f: impl FnOnce(&mut jni::JNIEnv, &jni::objects::JObject<'static>) -> Result<R, Error>,
    ) -> Result<R, Error> {
        let ctx = ndk_context::android_context();
        // Safety: as documented in `ndk-context` to obtain the `jni::JavaVM``
        let jvm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }
            .map_err(|_| Error::other("Failed to get `jni::JavaVM`"))?;
        // Safety: as documented in `cargo-apk` example to obtain the context's JNI reference
        let context = unsafe { jni::objects::JObject::from_raw(ctx.context().cast()) };
        let mut env = jvm
            .attach_current_thread()
            .map_err(|_| Error::other("Failed to attach the current thread with JVM"))?;
        f(&mut env, &context)
    }

    // `JObject` (got from `call_method()`) causes local reference overflow, wrap it with `AutoLocal`.
    #[inline(always)]
    pub(crate) fn jni_call_ret_obj<'local, 'other_local, O>(
        env: &mut jni::JNIEnv<'local>,
        obj: O,
        name: &str,
        sig: &str,
        args: &[jni::objects::JValueGen<&jni::objects::JObject<'other_local>>],
    ) -> Result<jni::objects::AutoLocal<'local, jni::objects::JObject<'local>>, Error>
    where
        O: AsRef<jni::objects::JObject<'other_local>>,
    {
        env.call_method(obj, name, sig, args)
            .and_then(|o| o.l())
            .map(|o| env.auto_local(o))
            .map_err(jerr)
    }

    /// Error converter, necessary for handling Java exceptions.
    #[inline]
    pub(crate) fn jerr(e: jni::errors::Error) -> Error {
        if let jni::errors::Error::JavaException = e {
            let _ = with_jni_env_ctx(|env, _| {
                let _ = env.exception_describe();
                if env.exception_occurred().is_ok() {
                    env.exception_clear().unwrap(); // panic if unable to clear
                }
                Ok(())
            });
        }
        Error::other(e)
    }

    #[inline]
    pub(crate) fn android_api_level() -> i32 {
        use std::sync::OnceLock;
        static API_LEVEL: OnceLock<i32> = OnceLock::new();
        *API_LEVEL.get_or_init(|| {
            with_jni_env_ctx(|env, _| {
                // the version can be read from `android_activity` or `ndk_sys`,
                // but here it tries to avoid such dependency or making unsafe calls.
                let os_build_class = env.find_class("android/os/Build$VERSION").map_err(jerr)?;
                env.get_static_field(os_build_class, "SDK_INT", "I")
                    .and_then(|v| v.i())
                    .map_err(jerr)
            })
            .unwrap_or(1)
        })
    }
}
