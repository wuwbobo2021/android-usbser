use core::slice;
use jni::objects::{AutoLocal, JObject, JPrimitiveArray, JString, JValue};
use std::time::Duration;

use crate::usb::{
    android_api_level, ctrl_request_dir, jerr, jni_call_ret_obj, with_jni_env,
    with_jni_env_activity, ConfigurationInfo, DeviceInfo, EndpointDirection, EndpointInfo, Error,
    InterfaceInfo,
};

/// Enumerates for all USB devices via Android Java API.
#[inline(always)]
pub fn list_devices() -> Result<Vec<DeviceInfo>, Error> {
    usb_manager()?.get_device_list()
}

/// Checks if the Android application is opened by an intent with
/// `android.hardware.usb.action.USB_DEVICE_ATTACHED`. If so, it takes the `DeviceInfo`
/// for the caller to open the device.
///
/// Please check it only on startup, in this case `has_permission()` usually returns `true`.
/// Otherwise, it might keep a invalid value after disconnection, but the permission is lost
/// even if the device connects again and gets the same filesystem path.
pub fn check_attached_intent() -> Result<DeviceInfo, Error> {
    usb_manager()?.check_attached_intent()
}

impl DeviceInfo {
    /// Returns true if the caller has permission to access the device.
    pub fn has_permission(&self) -> Result<bool, Error> {
        usb_manager()?.has_permission(self)
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
    /// receive the denied result for such thread to exit the loop (otherwise, poll for Resume).
    pub fn request_permission(&self) -> Result<(), Error> {
        usb_manager()?.request_permission(self)
    }

    /// Opens the device. Returns `Error::Access` if the permission is not granted.
    pub fn open_device(&self) -> Result<DeviceHandle, Error> {
        usb_manager()?.open_device(self)
    }

    /// Checks if the device is still in the list of connected devices.
    /// Note: The implementation can be optimized.
    #[inline(always)]
    pub fn check_connection(&self) -> bool {
        let vec_dev = list_devices().unwrap_or_default(); // heavy
        vec_dev.into_iter().any(|ref d| d == self)
    }
}

#[inline(always)]
fn usb_manager() -> Result<UsbManager, Error> {
    use std::sync::OnceLock;
    static USB_MAN: OnceLock<UsbManager> = OnceLock::new();
    if let Some(ref_man) = USB_MAN.get() {
        Ok(ref_man.clone())
    } else {
        let usb_man = UsbManager::build()?;
        let _ = USB_MAN.set(usb_man.clone());
        Ok(usb_man)
    }
}

/// Handles `android.hardware.usb.UsbManager`.
#[derive(Clone, Debug)]
pub(crate) struct UsbManager {
    pub(crate) internal: jni::objects::GlobalRef,
}

impl UsbManager {
    pub fn build() -> Result<Self, Error> {
        with_jni_env_activity(|env, activity| {
            // Query the global USB Service
            let class_android_ctx = env
                .find_class("android/content/Context")
                .map(|o| env.auto_local(o))
                .map_err(jerr)?;
            let field_usb_service = env
                .get_static_field(class_android_ctx, "USB_SERVICE", "Ljava/lang/String;")
                .and_then(|o| o.l())
                .map(|o| env.auto_local(o))
                .map_err(|_| Error::NotSupported)?;
            let ref_usb_manager = jni_call_ret_obj(
                env,
                activity,
                "getSystemService",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                &[(&field_usb_service).into()],
            )
            .map_err(|_| Error::Access)?;
            let usb_manager = env.new_global_ref(&ref_usb_manager).map_err(jerr)?;
            Ok(Self {
                internal: usb_manager,
            })
        })
    }

    pub fn has_permission(&self, device: &DeviceInfo) -> Result<bool, Error> {
        with_jni_env(|env| {
            env.call_method(
                &self.internal,
                "hasPermission",
                "(Landroid/hardware/usb/UsbDevice;)Z",
                &[device.internal.as_obj().into()],
            )
            .and_then(|b| b.z())
            .map_err(jerr)
        })
    }

    // For those who want to implement a synchronous function for permission request:
    // check Flutter's <https://github.com/altera2015/usbserial>, `lib/usb_serial.dart`,
    // `android/src/main/java/dev/bessems/usbserial/UsbSerialPlugin.java`:
    // The `BroadcastReceiver` sets `io.flutter.plugin.common.MethodChannel.Result`
    // (on receive) for the `await` call at the Flutter side to return.
    pub fn request_permission(&self, device: &DeviceInfo) -> Result<(), Error> {
        if self.has_permission(device)? {
            return Ok(());
        }
        with_jni_env_activity(|env, activity| {
            // Note: currently there is no Java helper defining the broadcast receiver,
            // thus lots of "useless" things are done for calling `requestPermission()`.
            let str_perm = env
                .new_string("com.example.android_usbser.USB_PERMISSION")
                .map(|o| env.auto_local(o))
                .map_err(jerr)?;
            let package_name =
                jni_call_ret_obj(env, activity, "getPackageName", "()Ljava/lang/String;", &[])?;

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
                    activity.into(),
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
                self.internal.as_obj(),
                "requestPermission",
                "(Landroid/hardware/usb/UsbDevice;Landroid/app/PendingIntent;)V",
                &[(&device.internal).into(), (&pending).into()],
            )
            .map(|_| ())
            .map_err(jerr)
            .map_err(|_| Error::Access)
        })
    }

    pub fn open_device(&self, device: &DeviceInfo) -> Result<DeviceHandle, Error> {
        if !self.has_permission(device)? {
            return Err(Error::Access);
        }
        with_jni_env(|env| {
            let conn = jni_call_ret_obj(
                env,
                &self.internal,
                "openDevice",
                "(Landroid/hardware/usb/UsbDevice;)Landroid/hardware/usb/UsbDeviceConnection;",
                &[(&device.internal).into()],
            )?;
            DeviceHandle::build(env, device, &conn)
        })
    }

    pub fn check_attached_intent(&self) -> Result<DeviceInfo, Error> {
        // Note: `getIntent()` and `setIntent()` are functions of `Activity` (not `Context`)
        with_jni_env_activity(|env, activity| {
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
                return Err(Error::NoDevice);
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
                return Err(Error::NoDevice);
            }
            let java_dev = jni_call_ret_obj(
                env,
                &intent_startup,
                "getParcelableExtra",
                // `(Ljava/lang/String;)java/lang/Object` triggers `java.lang.NoSuchMethodError`
                // TODO: add the class object parameter for API level 33 and above.
                "(Ljava/lang/String;)Landroid/os/Parcelable;",
                &[(&intent_extra_device).into()],
            )?;
            DeviceInfo::build(env, &java_dev)
        })
    }
}

/// Handles `android.hardware.usb.UsbDeviceConnection`.
/// It is capable of performing control transfer and bulk transfer.
/// Note: zero `timeout` duration indicates infinite timeout.
#[derive(Debug)]
pub struct DeviceHandle {
    internal: jni::objects::GlobalRef,
    dev_info: DeviceInfo,
    jmethod_control_transfer: jni::objects::JMethodID,
    jmethod_bulk_transfer: jni::objects::JMethodID,
}

impl DeviceHandle {
    fn build(env: &mut jni::JNIEnv, dev: &DeviceInfo, conn: &JObject<'_>) -> Result<Self, Error> {
        let jclass = env.get_object_class(conn).map_err(jerr)?;

        let jmethod_control_transfer = env
            .get_method_id(&jclass, "controlTransfer", "(IIII[BII)I")
            .map_err(jerr)?;

        let jmethod_bulk_transfer = env
            .get_method_id(
                &jclass,
                "bulkTransfer",
                "(Landroid/hardware/usb/UsbEndpoint;[BII)I",
            )
            .map_err(jerr)?;

        Ok(Self {
            internal: env.new_global_ref(conn).map_err(jerr)?,
            dev_info: dev.clone(),
            jmethod_control_transfer,
            jmethod_bulk_transfer,
        })
    }

    /// Gets the corresponding `DeviceInfo`.
    #[inline(always)]
    pub fn device_info(&self) -> &DeviceInfo {
        &self.dev_info
    }

    /// Checks if the device is still in the list of connected devices.
    /// Note: It is not checked automatically (for performance concerns);
    /// with the current implementation, in case of the device connects
    /// again and gets the same filesystem path, it may return a fake `true`.
    #[inline(always)]
    pub fn check_connection(&self) -> bool {
        self.dev_info.check_connection()
    }

    /// Sets the current USB device configuration. Requires API Level 21 and later.
    pub fn set_configuration(&self, conf: &ConfigurationInfo) -> Result<(), Error> {
        with_jni_env(|env| {
            env.call_method(
                &self.internal,
                "setConfiguration",
                "(Landroid/hardware/usb/UsbConfiguration;)Z",
                &[JValue::Object(conf.internal.as_obj())],
            )
            .and_then(|r| r.z())
            .map_err(jerr)
            .map_err(|_| Error::NotSupported)?
            .then_some(())
            .ok_or(Error::NotSet)
        })
    }

    /// Used to select between two interfaces with the same ID but
    /// different alternate setting. Requires API Level 21 and later.
    pub fn set_interface(&self, intr: &InterfaceInfo) -> Result<(), Error> {
        with_jni_env(|env| {
            env.call_method(
                &self.internal,
                "setInterface",
                "(Landroid/hardware/usb/UsbInterface;)Z",
                &[JValue::Object(intr.internal.as_obj())],
            )
            .and_then(|r| r.z())
            .map_err(jerr)
            .map_err(|_| Error::NotSupported)?
            .then_some(())
            .ok_or(Error::NotSet)
        })
    }

    /// Claims exclusive access to an interface. This must be done before
    /// sending or receiving data on any endpoint belonging to the interface.
    /// Returns `Error::Access` if `claimInterface()` returns `false`.
    ///
    /// `forced`: true to disconnect kernel driver if necessary.
    pub fn claim_interface(&self, intr: &InterfaceInfo, forced: bool) -> Result<(), Error> {
        with_jni_env(|env| {
            env.call_method(
                &self.internal,
                "claimInterface",
                "(Landroid/hardware/usb/UsbInterface;Z)Z",
                &[intr.internal.as_obj().into(), forced.into()],
            )
            .and_then(|r| r.z())
            .map_err(jerr)?
            .then_some(())
            .ok_or(Error::Access)
        })
    }

    /// Releases exclusive access to an interface.
    pub fn release_interface(&self, intr: &InterfaceInfo) -> Result<(), Error> {
        with_jni_env(|env| {
            env.call_method(
                &self.internal,
                "releaseInterface",
                "(Landroid/hardware/usb/UsbInterface;)Z",
                &[intr.internal.as_obj().into()],
            )
            .and_then(|r| r.z())
            .map_err(jerr)?
            .then_some(())
            .ok_or(Error::Access)
        })
    }

    /// Releases all system resources related to the device.
    /// Note: It may not be called automatically, neither in the Rust `drop()`
    /// nor in the Java `finalize()`. It is probably about the possible
    /// usage of the raw file descriptor.
    pub fn close(self) {
        let _ = with_jni_env(|env| {
            let _ = env
                .call_method(&self.internal, "close", "()V", &[])
                .map_err(jerr)?; // it might print possible JavaException
            Ok(())
        });
    }

    /// Control IN transaction on endpoint 0, reads data from device to host.
    #[inline]
    pub fn read_control(
        &self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        buf: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, Error> {
        if ctrl_request_dir(request_type) != Some(EndpointDirection::In) {
            return Err(Error::BadParam);
        }
        with_jni_env(|env| {
            let (len, ret_jbuf) =
                self.control_transfer(env, request_type, request, value, index, buf, timeout)?;
            if len == 0 {
                return Ok(0);
            }
            // copy back
            let buf = unsafe { slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut i8, len) };
            env.get_byte_array_region(ret_jbuf, 0, buf).map_err(jerr)?;
            Ok(len)
        })
    }

    /// Control OUT transaction on endpoint 0, writes data to the device.
    #[inline]
    pub fn write_control(
        &self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        buf: &[u8],
        timeout: Duration,
    ) -> Result<usize, Error> {
        if ctrl_request_dir(request_type) != Some(EndpointDirection::Out) {
            return Err(Error::BadParam);
        }
        with_jni_env(|env| {
            self.control_transfer(env, request_type, request, value, index, buf, timeout)
                .map(|(len, _)| len)
        })
    }

    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    fn control_transfer<'a>(
        &self,
        env: &mut jni::JNIEnv<'a>,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        buf: &[u8],
        timeout: Duration,
    ) -> Result<(usize, AutoLocal<'a, JPrimitiveArray<'a, i8>>), Error> {
        use jni::{
            signature::*,
            sys::{jint, jvalue},
        };
        let buf_len = buf.len().min(jint::MAX as usize) as jint;
        // TODO: is 0 size array always safe?
        let jbuf = if ctrl_request_dir(request_type) == Some(EndpointDirection::In) {
            env.new_byte_array(buf_len)
        } else {
            env.byte_array_from_slice(buf)
        }
        .map(|o| env.auto_local(o))
        .map_err(jerr)?;
        unsafe {
            env.call_method_unchecked(
                &self.internal,
                self.jmethod_control_transfer,
                ReturnType::Primitive(Primitive::Int),
                &[
                    jvalue {
                        i: request_type as jint,
                    },
                    jvalue { i: request as jint },
                    jvalue { i: value as jint },
                    jvalue { i: index as jint },
                    jvalue { l: jbuf.as_raw() },
                    jvalue { i: buf_len },
                    jvalue {
                        i: timeout.as_millis().min(i32::MAX as u128) as jint,
                    },
                ],
            )
            .and_then(|r| r.i())
            .map_err(jerr)
            .and_then(|l| {
                if l >= 0 {
                    Ok((l as usize, jbuf))
                } else {
                    Err(Error::Transfer(l))
                }
            })
        }
    }

    /// Performs a bulk IN transaction on the given endpoint.
    #[inline]
    pub fn read_bulk(
        &self,
        endpoint: &EndpointInfo,
        buf: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, Error> {
        if endpoint.direction() != EndpointDirection::In {
            return Err(Error::BadParam);
        }
        if buf.is_empty() {
            return Ok(0);
        }
        with_jni_env(|env| {
            let (len, ret_jbuf) = self.bulk_transfer(env, endpoint, buf, timeout)?;
            // copy back
            let buf = unsafe { slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut i8, len) };
            env.get_byte_array_region(&ret_jbuf, 0, buf).map_err(jerr)?;
            Ok(len)
        })
    }

    /// Performs a bulk OUT transaction on the given endpoint.
    #[inline]
    pub fn write_bulk(
        &self,
        endpoint: &EndpointInfo,
        buf: &[u8],
        timeout: Duration,
    ) -> Result<usize, Error> {
        if endpoint.direction() != EndpointDirection::Out {
            return Err(Error::BadParam);
        }
        if buf.is_empty() {
            return Ok(0);
        }
        with_jni_env(|env| {
            self.bulk_transfer(env, endpoint, buf, timeout)
                .map(|(len, _)| len)
        })
    }

    /// TODO: Make clear of possible usage of `JNIEnv::new_direct_byte_buffer()`
    /// (the relation between Java primitive array and `java.nio.ByteBuffer`).
    /// Simply calling `array()` on `ByteBuffer` created by `new_direct_byte_buffer()`
    /// may cause JavaExcecption, it can be UnsupportedOperationException.
    ///
    /// To avoid copying, just rewrite the function with `UsbRequest` and `requestWait()`,
    /// because `UsbRequest` handles `java.nio.ByteBuffer` instead of the primitive array.
    /// Note: According to `mik3y/usb-serial-for-android`, `requestWait()` *with*
    /// timeout parameter introduced in API 26 crashes with short timeout (up to 200 ms).
    #[inline(always)]
    fn bulk_transfer<'a>(
        &self,
        env: &mut jni::JNIEnv<'a>,
        endpoint: &EndpointInfo,
        buf: &[u8],
        timeout: Duration,
    ) -> Result<(usize, AutoLocal<'a, JPrimitiveArray<'a, i8>>), Error> {
        use jni::{
            signature::*,
            sys::{jint, jvalue},
        };
        let buf_len = buf.len().min(jint::MAX as usize) as jint;
        let jbuf = if endpoint.direction() == EndpointDirection::In {
            env.new_byte_array(buf_len)
        } else {
            env.byte_array_from_slice(buf)
        }
        .map(|o| env.auto_local(o))
        .map_err(jerr)?;
        unsafe {
            env.call_method_unchecked(
                &self.internal,
                self.jmethod_bulk_transfer,
                ReturnType::Primitive(Primitive::Int),
                &[
                    jvalue {
                        l: endpoint.internal.as_raw(),
                    },
                    jvalue { l: jbuf.as_raw() },
                    jvalue { i: buf_len },
                    jvalue {
                        i: timeout.as_millis().min(i32::MAX as u128) as jint,
                    },
                ],
            )
            .and_then(|r| r.i())
            .map_err(jerr)
            .and_then(|l| {
                if l >= 0 {
                    Ok((l as usize, jbuf))
                } else {
                    Err(Error::Transfer(l))
                }
            })
        }
    }

    /// Gets the OS raw file descriptor number.
    ///
    /// # Safety
    /// It is up to yourself. Do not call functions here
    /// while the file descriptor is being used elsewhere.
    pub unsafe fn get_raw_fd(&self) -> Result<i32, Error> {
        with_jni_env(|env| {
            env.call_method(&self.internal, "getFileDescriptor", "()I", &[])
                .and_then(|n| n.i())
                .map_err(jerr)
                .map_err(|_| Error::Access)
        })
    }
}

#[cfg(feature = "unsafe-rusb")]
impl DeviceHandle {
    /// # Safety
    ///
    /// Experimental. Currently untested function, it may be error-prone by itself.
    /// Debug for specific unrooted device! `libusb_wrap_sys_device()` may fail.
    ///
    /// After wrapping, `rusb` assumes ownership of the handle, and will close it on drop.
    pub unsafe fn create_rusb_connection(
        &self,
        rusb_context: &mut rusb::Context,
    ) -> Result<rusb::DeviceHandle<rusb::Context>, rusb::Error> {
        let mut raw_fd =
            self.get_raw_fd().map_err(|_| rusb::Error::BadDescriptor)? as std::ffi::c_int;

        let mut p_libusb_hdl =
            std::mem::MaybeUninit::<*mut rusb::ffi::libusb_device_handle>::uninit();
        unsafe {
            use rusb::UsbContext; // trait
            rusb_context.set_log_level(rusb::LogLevel::Debug);
            eprintln!("before libusb_wrap_sys_device(). fd: {:#010x}", raw_fd);
            check_libusb_error(rusb::ffi::libusb_wrap_sys_device(
                rusb_context.as_raw(),
                &mut raw_fd,
                p_libusb_hdl.as_mut_ptr(),
            ))?;
            eprintln!("libusb_wrap_sys_device() success.");
            let libusb_hdl =
                std::ptr::NonNull::new(p_libusb_hdl.assume_init()).ok_or(rusb::Error::NoDevice)?;
            Ok(rusb::DeviceHandle::from_libusb(
                rusb_context.clone(),
                libusb_hdl,
            ))
        }
    }
}

/// Create new `rusb` context with option `LIBUSB_OPTION_NO_DEVICE_DISCOVERY`.
///
/// # Safety
/// Use with extra care.
#[cfg(feature = "unsafe-rusb")]
pub unsafe fn create_rusb_context() -> Result<rusb::Context, rusb::Error> {
    use rusb::ffi::*;
    // creates a null pointer of type `libusb_context`
    let mut context = std::mem::MaybeUninit::<*mut libusb_context>::uninit();
    // according to `libusb` docs, `libusb_set_option` doesn't use the context
    check_libusb_error(libusb_set_option(
        *context.as_ptr(),
        constants::LIBUSB_OPTION_NO_DEVICE_DISCOVERY,
    ))?;
    // allocates the libusb context object by `libusb_init()`, sets the pointer `context`
    check_libusb_error(libusb_init(context.as_mut_ptr()))?;
    let context = context.assume_init();
    // according to `rusb` docs, this transfers ownership of the context to Rust
    Ok(rusb::Context::from_raw(context))
}

// Based on `rusb/src/error.rs`
#[cfg(feature = "unsafe-rusb")]
fn check_libusb_error(err: i32) -> Result<(), rusb::Error> {
    use rusb::constants::*;
    if err == 0 {
        return Ok(());
    }
    let error = match err {
        LIBUSB_ERROR_IO => rusb::Error::Io,
        LIBUSB_ERROR_INVALID_PARAM => rusb::Error::InvalidParam,
        LIBUSB_ERROR_ACCESS => rusb::Error::Access,
        LIBUSB_ERROR_NO_DEVICE => rusb::Error::NoDevice,
        LIBUSB_ERROR_NOT_FOUND => rusb::Error::NotFound,
        LIBUSB_ERROR_BUSY => rusb::Error::Busy,
        LIBUSB_ERROR_TIMEOUT => rusb::Error::Timeout,
        LIBUSB_ERROR_OVERFLOW => rusb::Error::Overflow,
        LIBUSB_ERROR_PIPE => rusb::Error::Pipe,
        LIBUSB_ERROR_INTERRUPTED => rusb::Error::Interrupted,
        LIBUSB_ERROR_NO_MEM => rusb::Error::NoMem,
        LIBUSB_ERROR_NOT_SUPPORTED => rusb::Error::NotSupported,
        _ => rusb::Error::Other,
    };
    Err(error)
}
