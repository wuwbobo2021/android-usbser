//! Android USB serial driver, currently works with CDC-ACM devices.
//!
//! Reference:
//! - <https://developer.android.com/develop/connectivity/usb/host>
//! - <https://developer.android.com/reference/android/hardware/usb/package-summary>
//! - <https://github.com/mik3y/usb-serial-for-android>
//!
//! It is far from being feature-complete. Of course you can use something like
//! [react-native-usb-serialport](https://www.npmjs.com/package/react-native-usb-serialport),
//! however, that would be yet another layer between Rust and Java.
//!
//! This crate requires `ndk_context::AndroidContext`, usually initialized by
//! crate `android_activity`. `jni::JavaVM::attach_current_thread_permanently()` is called.

mod ser_cdc;
mod usb_conn;
mod usb_info;
pub use ser_cdc::*;

/// Android USB Host API wrapper.
///
/// Structs with `Info` suffix are created during enumeration, their fields
/// except the list of subelements are read on creation (no updates).
/// `PartialEq` impls also rely on these read values, except for `DeviceInfo`.
///
/// Raw file descriptor can be obtained from `DeviceHandle`,
/// it may be used by crates like `nusb` or `rusb` (at your own risk).
///
/// TODO: Use `android.hardware.usb.UsbRequest` to avoid deep copying and provide
/// asynchronous features (crate `btleplug` can be checked for reference).
/// Then, make it merge into `nusb`.
pub mod usb {
    pub use crate::usb_conn::*;
    pub use crate::usb_info::*;

    /// Note: it assumes the current context is initialized by android-activity,
    /// then `with_jni_env_activity()` calls throughout the code acutally uses
    /// handlers of the same Android context (the handler `AndroidContext` is `Copy`).
    #[inline(always)]
    pub(crate) fn with_jni_env_activity<R>(
        f: impl FnOnce(&mut jni::JNIEnv, &jni::objects::JObject<'static>) -> Result<R, Error>,
    ) -> Result<R, Error> {
        let ctx = ndk_context::android_context();
        let jvm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }.map_err(|_| Error::JniEnv)?;
        let context = unsafe { jni::objects::JObject::from_raw(ctx.context().cast()) };
        let mut env = jvm
            .attach_current_thread_permanently()
            .map_err(|_| Error::JniEnv)?;
        f(&mut env, &context)
    }

    // Note: different threads must use different `JNIEnv` while sharing the same JVM.
    #[inline(always)]
    pub(crate) fn with_jni_env<R>(
        f: impl FnOnce(&mut jni::JNIEnv) -> Result<R, Error>,
    ) -> Result<R, Error> {
        let ctx = ndk_context::android_context();
        let jvm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }.map_err(|_| Error::JniEnv)?;
        let mut env = jvm
            .attach_current_thread_permanently()
            .map_err(|_| Error::JniEnv)?;
        f(&mut env)
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

    /// Possible errors returned by this module.
    ///
    /// Note: Error handlers may be improved throughout the source code.
    #[derive(Clone, Debug)]
    pub enum Error {
        /// Permission denied.
        Access,
        /// USB transfer failed, negative value returned from `UsbDeviceConnection`.
        Transfer(i32),
        /// Bad function parameter.
        BadParam,
        /// Failed to get JNI environment or the current context (native activity).
        JniEnv,
        /// A setter function of the Java object returned false.
        NotSet,
        /// The operation is probably not supported in the API of the current OS.
        NotSupported,
        /// Device not found.
        NoDevice,
        /// Unexpected Java exception.
        JavaException(String),
        /// Unexpected Error, including JNI issues.
        Other(String),
    }

    impl std::fmt::Display for Error {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            use Error::*;
            match self {
                Access => write!(f, "Permission denied"),
                Transfer(n) => write!(f, "USB transfer failed, {} from UsbDeviceConnection", n),
                BadParam => write!(f, "Bad function parameter passed (android-usbser)"),
                JniEnv => write!(f, "Failed to get JNI environment or the current context"),
                NotSet => write!(f, "A setter function of the Java object returned false"),
                NotSupported => write!(f, "Probably not supported in current OS"),
                NoDevice => write!(f, "Device not found"),
                JavaException(ref s) => write!(f, "Unexpected Java exception: {}", s),
                Other(ref s) => write!(f, "Unexpected error: {}", s),
            }
        }
    }
    impl std::error::Error for Error {}

    /// Error converter, necessary for handling Java exceptions.
    pub(crate) fn jerr(e: jni::errors::Error) -> Error {
        match e {
            jni::errors::Error::JavaException => {
                with_jni_env(|env| {
                    let _ = env.exception_describe();
                    if env.exception_occurred().is_ok() {
                        env.exception_clear().unwrap(); // panic if unable to clear
                        Ok(Error::JavaException(format!("{:?}", e)))
                    } else {
                        Ok(Error::JavaException(String::new()))
                    }
                })
                .unwrap()
            }
            _ => Error::Other(format!("{:?}", e)),
        }
    }

    pub(crate) fn android_api_level() -> i32 {
        use std::sync::OnceLock;
        static API_LEVEL: OnceLock<i32> = OnceLock::new();
        *API_LEVEL.get_or_init(|| {
            with_jni_env(|env| {
                // the version can be read from `android_activity` or `ndk_sys`,
                // but here it tries to avoid such dependency or making unsafe calls.
                // Where is the specification about `$` as a mark for nested classes?
                let os_build_class = env.find_class("android/os/Build$VERSION").map_err(jerr)?;
                env.get_static_field(os_build_class, "SDK_INT", "I")
                    .and_then(|v| v.i())
                    .map_err(jerr)
            })
            .unwrap_or(1)
        })
    }
}
