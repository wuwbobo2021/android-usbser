use crate::usb::{jerr, usb_manager, Error};
use getset::*;
use jni::{objects::JObject, sys::jint, JNIEnv};
use jni_min_helper::*;

/// Enumerates for all USB devices via Android Java API.
pub fn list_devices() -> Result<Vec<DeviceInfo>, Error> {
    let usb_man = usb_manager()?;
    let env = &mut jni_attach_vm().map_err(jerr)?;
    let mut devices = Vec::new();
    let ref_dev_list = env
        .call_method(&usb_man, "getDeviceList", "()Ljava/util/HashMap;", &[])
        .get_object(env)
        .map_err(jerr)?;
    let map_dev = env.get_map(&ref_dev_list).map_err(jerr)?;
    let mut iter_dev = map_dev.iter(env).map_err(jerr)?;
    while let Some((name, dev)) = iter_dev.next(env).map_err(jerr)? {
        devices.push(DeviceInfo::build(env, &dev)?);
        drop((env.auto_local(name), env.auto_local(dev)));
    }
    Ok(devices)
}

/// Corresponds to `android.hardware.usb.UsbDevice`.
/// Its fields and the `InterfaceInfo` list are read on creation and will not
/// be updated automatically; however, `PartialEq` depends on these fields.
#[derive(Clone, CopyGetters, Getters)]
pub struct DeviceInfo {
    pub(crate) internal: jni::objects::GlobalRef,

    /// Equals `idVendor`.
    #[getset(get_copy = "pub")]
    vendor_id: u16,
    /// Equals `idProduct`.
    #[getset(get_copy = "pub")]
    product_id: u16,
    /// Equals `bDeviceClass`.
    #[getset(get_copy = "pub")]
    class: u8,
    /// Equals `bDeviceSubClass`.
    #[getset(get_copy = "pub")]
    subclass: u8,
    /// Equals `bDeviceProtocol`.
    #[getset(get_copy = "pub")]
    protocol: u8,

    /// (usually) Path of the device in the usbfs file system.
    #[getset(get = "pub")]
    path_name: String,
    /// Vendor name.
    #[getset(get = "pub")]
    manufacturer_string: Option<String>,
    /// Product name.
    #[getset(get = "pub")]
    product_string: Option<String>,
    /// USB protocol version.
    #[getset(get = "pub")]
    version: Option<String>,
    /// Device serial ID string.
    #[getset(get = "pub")]
    serial_number: Option<String>,

    interfaces: Vec<InterfaceInfo>,
}

impl DeviceInfo {
    pub(crate) fn build(env: &mut JNIEnv, dev: &JObject<'_>) -> Result<Self, Error> {
        let num_interfaces = get_int_field(env, dev, "getInterfaceCount")? as u8;
        let mut interface_refs = Vec::new();
        for i in 0..num_interfaces {
            interface_refs.push(
                env.call_method(
                    dev,
                    "getInterface",
                    "(I)Landroid/hardware/usb/UsbInterface;",
                    &[(i as jint).into()],
                )
                .get_object(env)
                .map_err(jerr)?,
            );
        }
        let mut info = Self {
            internal: env.new_global_ref(dev).map_err(jerr)?,

            vendor_id: get_int_field(env, dev, "getVendorId")? as u16,
            product_id: get_int_field(env, dev, "getProductId")? as u16,
            class: get_int_field(env, dev, "getDeviceClass")? as u8,
            subclass: get_int_field(env, dev, "getDeviceSubclass")? as u8,
            protocol: get_int_field(env, dev, "getDeviceProtocol")? as u8,

            path_name: get_string_field(env, dev, "getDeviceName")?,
            manufacturer_string: None,
            product_string: None,
            version: None,
            serial_number: None,

            interfaces: {
                let mut interfaces = Vec::new();
                for interface in interface_refs.into_iter() {
                    interfaces.push(InterfaceInfo {
                        interface_number: get_int_field(env, &interface, "getId")? as u8,
                        class: get_int_field(env, &interface, "getInterfaceClass")? as u8,
                        sub_class: get_int_field(env, &interface, "getInterfaceSubclass")? as u8,
                        protocol: get_int_field(env, &interface, "getInterfaceProtocol")? as u8,
                        num_endpoints: get_int_field(env, &interface, "getEndpointCount")? as u8,
                    });
                }
                interfaces
            },
        };
        if android_api_level() >= 21 {
            info.version = Some(get_string_field(env, dev, "getVersion")?);
            info.manufacturer_string = get_string_field(env, dev, "getManufacturerName").ok();
            info.product_string = get_string_field(env, dev, "getProductName").ok();
            info.serial_number = get_string_field(env, dev, "getSerialNumber").ok();
        }
        Ok(info)
    }

    /// Iterator over the device's interfaces.
    pub fn interfaces(&self) -> impl Iterator<Item = &InterfaceInfo> {
        self.interfaces.iter()
    }
}

impl std::fmt::Debug for DeviceInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = f.debug_struct("DeviceInfo");

        s.field("vendor_id", &format_args!("0x{:04X}", self.vendor_id));
        s.field("product_id", &format_args!("0x{:04X}", self.product_id));
        s.field("class", &format_args!("0x{:02X}", self.class));
        s.field("subclass", &format_args!("0x{:02X}", self.subclass));
        s.field("protocol", &format_args!("0x{:02X}", self.protocol));

        s.field("path_name", &self.path_name);
        s.field("version", &self.version);
        s.field("manufacturer_string", &self.manufacturer_string);
        s.field("product_string", &self.product_string);
        s.field("serial_number", &self.serial_number);

        for intr in self.interfaces.iter() {
            s.field("Interface", &intr);
        }
        s.finish()
    }
}

impl PartialEq for DeviceInfo {
    fn eq(&self, other: &Self) -> bool {
        // Check `android.hardware.usb.UsbDevice.equals()` source code:
        // it may compare both `UsbDevice` only by name (`path_name`).
        self.vendor_id == other.vendor_id
            && self.product_id == other.product_id
            && self.path_name == other.path_name
            && self.serial_number == other.serial_number
    }
}

/// Corresponds to `android.hardware.usb.UsbInterface`.
#[derive(Clone, Copy, CopyGetters)]
#[getset(get_copy = "pub")]
pub struct InterfaceInfo {
    /// Equals `bInterfaceNumber`.
    interface_number: u8,
    /// Equals `bInterfaceClass`.
    class: u8,
    /// Equals `bInterfaceSubClass`.
    sub_class: u8,
    /// Equals `bInterfaceProtocol`.
    protocol: u8,
    /// Equals `bNumEndpoints`.
    num_endpoints: u8,
}

impl std::fmt::Debug for InterfaceInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InterfaceInfo")
            .field("interface_number", &self.interface_number)
            .field("class", &format_args!("0x{:02X}", self.class))
            .field("sub_class", &format_args!("0x{:02X}", self.sub_class))
            .field("protocol", &format_args!("0x{:02X}", self.protocol))
            .field("num_endpoints", &self.num_endpoints)
            .finish()
    }
}

// These functions call java methods without parameter. Error::Other on failure.
#[inline(always)]
fn get_int_field(env: &mut JNIEnv, dev: &JObject<'_>, method: &str) -> Result<jint, Error> {
    env.call_method(dev, method, "()I", &[])
        .get_int()
        .map_err(jerr)
}
#[inline(always)]
fn get_string_field(env: &mut JNIEnv, dev: &JObject<'_>, method: &str) -> Result<String, Error> {
    env.call_method(dev, method, "()Ljava/lang/String;", &[])
        .get_object(env)
        .and_then(|o| o.get_string(env))
        .map_err(jerr)
}
