use jni::objects::{JString, JValue};

use crate::Error;
use std::io::ErrorKind;

use crate::usb::{
    android_api_level, jerr, jni_call_ret_obj, list_devices, with_jni_env_ctx, DeviceInfo,
};

/// Gets a gloabal reference of `android.hardware.usb.UsbManager`.
#[inline(always)]
pub(crate) fn usb_manager() -> Result<jni::objects::GlobalRef, Error> {
    use std::sync::OnceLock;
    static USB_MAN: OnceLock<jni::objects::GlobalRef> = OnceLock::new();
    if let Some(ref_man) = USB_MAN.get() {
        Ok(ref_man.clone())
    } else {
        let usb_man = get_usb_manager()?;
        let _ = USB_MAN.set(usb_man.clone());
        Ok(usb_man)
    }
}
fn get_usb_manager() -> Result<jni::objects::GlobalRef, Error> {
    with_jni_env_ctx(|env, ctx| {
        // Query the global USB Service
        let class_android_ctx = env
            .find_class("android/content/Context")
            .map(|o| env.auto_local(o))
            .map_err(jerr)?;
        let field_usb_service = env
            .get_static_field(class_android_ctx, "USB_SERVICE", "Ljava/lang/String;")
            .and_then(|o| o.l())
            .map(|o| env.auto_local(o))
            .map_err(|_| Error::new(ErrorKind::Unsupported, "USB_SERVICE not found"))?;
        let ref_usb_manager = jni_call_ret_obj(
            env,
            ctx,
            "getSystemService",
            "(Ljava/lang/String;)Ljava/lang/Object;",
            &[(&field_usb_service).into()],
        )
        .map_err(|_| {
            Error::new(
                ErrorKind::PermissionDenied,
                "getSystemService(USB_SERVICE) failed",
            )
        })?;
        let usb_manager = env.new_global_ref(&ref_usb_manager).map_err(jerr)?;
        Ok(usb_manager)
    })
}

/// Checks if the Android application is opened by an intent with
/// `android.hardware.usb.action.USB_DEVICE_ATTACHED`. If so, it takes the `DeviceInfo`
/// for the caller to open the device.
///
/// Please check it only on startup, in this case `has_permission()` usually returns `true`.
/// Otherwise, it might keep a invalid value after disconnection, but the permission is lost
/// even if the device connects again and gets the same filesystem path.
pub fn check_attached_intent() -> Result<DeviceInfo, Error> {
    // Note: `getIntent()` and `setIntent()` are functions of `Activity` (not `Context`)
    with_jni_env_ctx(|env, activity| {
        // expected intent action
        let class_usb_man = env
            .find_class("android/hardware/usb/UsbManager")
            .map(|o| env.auto_local(o)) // find_class() returns a wrapper of JObject
            .map_err(jerr)?;
        let action_usb_device_attached = env
            .get_static_field(
                &class_usb_man,
                "ACTION_USB_DEVICE_ATTACHED",
                "Ljava/lang/String;",
            )
            .and_then(|o| o.l())
            .map(|o| env.auto_local(o))
            .map_err(jerr)?;

        // the Intent instance is taken from Activity by getIntent()
        let intent_startup = jni_call_ret_obj(
            env,
            activity,
            "getIntent",
            "()Landroid/content/Intent;",
            &[],
        )?;
        let action_startup = env
            .call_method(&intent_startup, "getAction", "()Ljava/lang/String;", &[])
            .and_then(|o| o.l())
            .map_err(jerr)
            .unwrap_or(JString::default().into());

        // checks if the action of current intent is ACTION_USB_DEVICE_ATTACHED
        let action_matches = env
            .call_method(
                &action_startup,
                "equals",
                "(Ljava/lang/Object;)Z",
                &[(&action_usb_device_attached).into()],
            )
            .and_then(|b| b.z())
            .map_err(jerr)
            .unwrap_or(false);
        drop(env.auto_local(action_startup));
        if !action_matches {
            let _ = env
                .call_method(
                    activity,
                    "setIntent",
                    "(Landroid/content/Intent;)V",
                    &[JValue::Object(&intent_startup)],
                )
                .map_err(jerr); // set it back, may fail
            return Err(Error::from(ErrorKind::NotFound));
        }

        // gets the `UsbDevice`
        let intent_extra_device = env
            .get_static_field(&class_usb_man, "EXTRA_DEVICE", "Ljava/lang/String;")
            .and_then(|o| o.l())
            .map(|o| env.auto_local(o))
            .map_err(jerr)?;
        // ensures the extra is not null
        let has_extra_device = env
            .call_method(
                &intent_startup,
                "hasExtra",
                "(Ljava/lang/String;)Z",
                &[(&intent_extra_device).into()],
            )
            .and_then(|o| o.z())
            .map_err(jerr)?;
        if !has_extra_device {
            return Err(Error::new(
                ErrorKind::NotFound,
                "Unexpected: Intent with ACTION_USB_DEVICE_ATTACHED have no EXTRA_DEVICE",
            ));
        }
        let java_dev = jni_call_ret_obj(
            env,
            &intent_startup,
            "getParcelableExtra",
            // TODO: this is deprecated in API 33 and above without the class parameter.
            "(Ljava/lang/String;)Landroid/os/Parcelable;",
            &[(&intent_extra_device).into()],
        )?;
        DeviceInfo::build(env, &java_dev)
    })
}

impl DeviceInfo {
    /// Returns true if the caller has permission to access the device.
    pub fn has_permission(&self) -> Result<bool, Error> {
        let usb_man = usb_manager()?;
        with_jni_env_ctx(|env, _| {
            env.call_method(
                &usb_man,
                "hasPermission",
                "(Landroid/hardware/usb/UsbDevice;)Z",
                &[self.internal.as_obj().into()],
            )
            .and_then(|b| b.z())
            .map_err(jerr)
        })
    }

    /// Checks if the device is still in the list of connected devices.
    /// Note: The implementation can be optimized.
    #[inline(always)]
    pub fn check_connection(&self) -> bool {
        let vec_dev = list_devices().unwrap_or_default(); // heavy
        vec_dev.into_iter().any(|ref d| d == self)
    }

    /// Non-blocking permission request.
    ///
    /// Polling for `has_permission()` in main thread after the request may never receive `true`,
    /// because application events are not being polled. Note: The activity might be paused by
    /// `requestPermission()` here, but resumed on receving result.
    ///
    /// `has_permission()` can be checked again on `android_activity::MainEvent::Resume`,
    /// Otherwise poll in a background thread (it wouldn't be paused/resumed automatically).
    /// TODO: Some Java helper handling `android.content.BroadcastReceiver` may be used to
    /// receive the denied result for such thread to exit the loop; otherwise, receive the
    /// signal from the main thread on Resume.
    ///
    /// A blocking permission request function may be added in the future.
    /// Reference: <https://pub.dev/packages/libusb_android_helper>.
    pub fn request_permission(&self) -> Result<(), Error> {
        if self.has_permission()? {
            return Ok(());
        }
        let usb_man = usb_manager()?;
        with_jni_env_ctx(|env, ctx| {
            // Note: currently there is no Java helper defining the broadcast receiver,
            // thus lots of "useless" things are done for calling `requestPermission()`.
            let str_perm = env
                .new_string("com.example.android_usbser.USB_PERMISSION")
                .map(|o| env.auto_local(o))
                .map_err(jerr)?;
            let package_name =
                jni_call_ret_obj(env, ctx, "getPackageName", "()Ljava/lang/String;", &[])?;

            let intent = env
                .new_object(
                    "android/content/Intent",
                    "(Ljava/lang/String;)V",
                    &[JValue::Object(&str_perm)],
                )
                .map(|o| env.auto_local(o))
                .map_err(jerr)?;
            let _ = jni_call_ret_obj(
                env,
                &intent,
                "setPackage",
                "(Ljava/lang/String;)Landroid/content/Intent;",
                &[(&package_name).into()],
            )?;

            let pending = env.call_static_method(
                "android/app/PendingIntent",
                "getBroadcast",
                "(Landroid/content/Context;ILandroid/content/Intent;I)Landroid/app/PendingIntent;",
                &[
                    ctx.into(),
                    0_i32.into(),
                    JValue::Object(&intent),
                    if android_api_level() < 31 {
                        0 // should it be FLAG_IMMUTABLE since API 23?
                    } else {
                        0x02000000 // FLAG_MUTABLE (since API 31, Android 12)
                    }.into()
                ]
            )
            .and_then(|o| o.l())
            .map(|o| env.auto_local(o))
            .map_err(jerr)?;

            env.call_method(
                &usb_man,
                "requestPermission",
                "(Landroid/hardware/usb/UsbDevice;Landroid/app/PendingIntent;)V",
                &[(&self.internal).into(), (&pending).into()],
            )
            .map(|_| ())
            .map_err(jerr)
            .map_err(|_| Error::other("Unexpected error from `requestPermission()`"))
        })
    }

    /// Opens the device. Returns error `PermissionDenied` if the permission is not granted.
    pub fn open_device(&self) -> Result<nusb::Device, Error> {
        if !self.has_permission()? {
            return Err(Error::from(ErrorKind::PermissionDenied));
        }
        let usb_man = usb_manager()?;
        let raw_fd = with_jni_env_ctx(|env, _| {
            let conn = jni_call_ret_obj(
                env,
                &usb_man,
                "openDevice",
                "(Landroid/hardware/usb/UsbDevice;)Landroid/hardware/usb/UsbDeviceConnection;",
                &[(&self.internal).into()],
            )
            .map_err(|_| Error::other("Unexpected error from `openDevice()`"))?;
            env.call_method(&conn, "getFileDescriptor", "()I", &[])
                .and_then(|n| n.i())
                .map_err(jerr)
        })?;
        // Safety: `close()` is not called automatically when the JNI `AutoLocal` of `conn`
        // and the corresponding Java object is destroyed. (check `UsbDeviceConnection` source)
        use std::os::fd::*;
        let owned_fd = unsafe { OwnedFd::from_raw_fd(raw_fd as RawFd) };
        nusb::Device::from_fd(owned_fd)
    }
}
