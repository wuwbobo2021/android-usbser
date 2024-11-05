use jni::{
    objects::{JObject, JString},
    sys::jint,
    JNIEnv,
};

use crate::usb::{android_api_level, jerr, jni_call_ret_obj, with_jni_env, Error, UsbManager};

impl UsbManager {
    pub fn get_device_list(&self) -> Result<Vec<DeviceInfo>, Error> {
        with_jni_env(|env| {
            let mut vec_dev = Vec::new();

            let ref_dev_list = jni_call_ret_obj(
                env,
                self.internal.as_obj(),
                "getDeviceList",
                "()Ljava/util/HashMap;",
                &[],
            )?;
            let map_dev = env.get_map(&ref_dev_list).map_err(jerr)?;
            let mut iter_dev = map_dev.iter(env).map_err(jerr)?;
            while let Some((name, dev)) = iter_dev.next(env).map_err(jerr)? {
                vec_dev.push(DeviceInfo::build(env, &dev)?);
                drop((env.auto_local(name), env.auto_local(dev)));
            }
            Ok(vec_dev)
        })
    }
}

use getset::*;

/// Corresponds to `android.hardware.usb.UsbDevice`.
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

    /// Equals `bNumConfigurations`.
    #[getset(get_copy = "pub")]
    num_configurations: Option<u8>,
    /// Equals `bNumInterfaces` in current configuration.
    #[getset(get_copy = "pub")]
    num_interfaces: u8,

    /// (usually) Raw device path in the filesystem.
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
}

impl DeviceInfo {
    pub(crate) fn build(env: &mut JNIEnv, dev: &JObject<'_>) -> Result<Self, Error> {
        let mut info = Self {
            internal: env.new_global_ref(dev).map_err(jerr)?,

            vendor_id: get_int_field(env, dev, "getVendorId")? as u16,
            product_id: get_int_field(env, dev, "getProductId")? as u16,
            class: get_int_field(env, dev, "getDeviceClass")? as u8,
            subclass: get_int_field(env, dev, "getDeviceSubclass")? as u8,
            protocol: get_int_field(env, dev, "getDeviceProtocol")? as u8,
            num_configurations: None,
            num_interfaces: get_int_field(env, dev, "getInterfaceCount")? as u8,

            path_name: get_string_field(env, dev, "getDeviceName")?,
            manufacturer_string: None,
            product_string: None,
            version: None,
            serial_number: None,
        };
        if android_api_level() >= 21 {
            info.num_configurations = Some(get_int_field(env, dev, "getConfigurationCount")? as u8);

            info.manufacturer_string = Some(get_string_field(env, dev, "getManufacturerName")?);
            info.product_string = Some(get_string_field(env, dev, "getProductName")?);
            info.version = Some(get_string_field(env, dev, "getVersion")?);
            info.serial_number = Some(get_string_field(env, dev, "getSerialNumber")?);
        }
        Ok(info)
    }

    pub fn get_interfaces(&self) -> Result<Vec<InterfaceInfo>, Error> {
        with_jni_env(|env| get_interfaces(env, &self.internal, self.num_interfaces))
    }

    /// Unoptimized convenient wrapper. Requires API Level 21 and later.
    pub fn get_active_configuration(&self) -> Result<ConfigurationInfo, Error> {
        let vec_conf = self.get_configurations()?;
        let cur_ints = self.get_interfaces()?;
        for conf in vec_conf {
            if conf.get_interfaces().unwrap_or_default() == cur_ints {
                return Ok(conf);
            }
        }
        Err(Error::Other("no configuration matches".to_string()))
    }

    /// Get configuration descriptors. Requires API Level 21 and later.
    pub fn get_configurations(&self) -> Result<Vec<ConfigurationInfo>, Error> {
        if android_api_level() < 21 {
            return Err(Error::NotSupported);
        }
        with_jni_env(|env| {
            let mut vec_conf = Vec::new();
            for i in 0..self.num_configurations.ok_or(Error::NotSupported)? {
                let conf = jni_call_ret_obj(
                    env,
                    &self.internal,
                    "getConfiguration",
                    "(I)Landroid/hardware/usb/UsbConfiguration;",
                    &[(i as jint).into()],
                )
                .map_err(|_| Error::NotSupported)?;
                vec_conf.push(ConfigurationInfo::build(env, &conf)?);
            }
            Ok(vec_conf)
        })
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
        s.field("num_configurations", &self.num_configurations);
        s.field("num_interfaces", &self.num_interfaces);

        s.field("path_name", &self.path_name);
        s.field("manufacturer_string", &self.manufacturer_string);
        s.field("product_string", &self.product_string);
        s.field("version", &self.version);
        s.field("serial_number", &self.serial_number);

        if let Ok(confs) = self.get_configurations() {
            for conf in confs {
                s.field("Configuration", &conf);
            }
        } else if let Ok(ints) = self.get_interfaces() {
            for intr in ints {
                s.field("Interface", &intr);
            }
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

#[derive(Clone, CopyGetters)]
#[getset(get_copy = "pub")]
/// Corresponds to `android.hardware.usb.UsbConfiguration` Added in API level 21.
pub struct ConfigurationInfo {
    #[getset(skip)]
    pub(crate) internal: jni::objects::GlobalRef,

    /// Equals `bConfigurationValue`.
    configuration_value: u8,
    /// Equals `bNumInterfaces`.
    num_interfaces: u8,
    /// Equals `bMaxPower`.
    max_power: u16,
    /// From `bmAttributes`.
    self_powered: bool,
    /// From `bmAttributes`.
    remote_wakeup: bool,
}

impl ConfigurationInfo {
    fn build(env: &mut JNIEnv, conf: &JObject<'_>) -> Result<Self, Error> {
        Ok(Self {
            internal: env.new_global_ref(conf).map_err(jerr)?,

            configuration_value: get_int_field(env, conf, "getId")? as u8,
            num_interfaces: get_int_field(env, conf, "getInterfaceCount")? as u8,

            max_power: get_int_field(env, conf, "getMaxPower")? as u16,
            self_powered: get_bool_field(env, conf, "isSelfPowered")?,
            remote_wakeup: get_bool_field(env, conf, "isRemoteWakeup")?,
        })
    }

    pub fn get_interfaces(&self) -> Result<Vec<InterfaceInfo>, Error> {
        with_jni_env(|env| get_interfaces(env, &self.internal, self.num_interfaces))
    }
}

impl std::fmt::Debug for ConfigurationInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = f.debug_struct("ConfigurationInfo");

        s.field("configuration_value", &self.configuration_value);
        s.field("num_interfaces", &self.num_interfaces);
        s.field("max_power", &format_args!("{} mA", self.max_power));
        s.field("self_powered", &self.self_powered);
        s.field("remote_wakeup", &self.remote_wakeup);

        if let Ok(ints) = self.get_interfaces() {
            for intr in ints {
                s.field("Interface", &intr);
            }
        }
        s.finish()
    }
}

impl PartialEq for ConfigurationInfo {
    fn eq(&self, other: &Self) -> bool {
        let equ = self.configuration_value == other.configuration_value
            && self.num_interfaces == other.num_interfaces
            && self.max_power == other.max_power
            && self.self_powered == other.self_powered
            && self.remote_wakeup == other.remote_wakeup;
        if !equ {
            return false;
        }
        let vec_ints = self.get_interfaces().unwrap_or_default();
        let vec_ints_other = other.get_interfaces().unwrap_or_default();
        vec_ints == vec_ints_other
    }
}

fn get_interfaces(
    env: &mut JNIEnv,
    java_obj: &JObject,
    num_ints: u8,
) -> Result<Vec<InterfaceInfo>, Error> {
    let mut vec_interface = Vec::new();
    for i in 0..num_ints {
        let interface = jni_call_ret_obj(
            env,
            java_obj,
            "getInterface",
            "(I)Landroid/hardware/usb/UsbInterface;",
            &[(i as jint).into()],
        )?;
        vec_interface.push(InterfaceInfo::build(env, &interface)?);
    }
    Ok(vec_interface)
}

/// Corresponds to `android.hardware.usb.UsbInterface`.
#[derive(Clone, CopyGetters)]
#[getset(get_copy = "pub")]
pub struct InterfaceInfo {
    #[getset(skip)]
    pub(crate) internal: jni::objects::GlobalRef,

    /// Equals `bInterfaceNumber`.
    interface_number: u8,
    /// Equals `bAlternateSetting`.
    alternate_setting: Option<u8>,
    /// Equals `bNumEndpoints`.
    num_endpoints: u8,
    /// Equals `bInterfaceClass`.
    class: u8,
    /// Equals `bInterfaceSubClass`.
    sub_class: u8,
    /// Equals `bInterfaceProtocol`.
    protocol: u8,
}

impl InterfaceInfo {
    fn build(env: &mut JNIEnv, interface: &JObject<'_>) -> Result<Self, Error> {
        Ok(Self {
            internal: env.new_global_ref(interface).map_err(jerr)?,

            interface_number: get_int_field(env, interface, "getId")? as u8,
            alternate_setting: (android_api_level() >= 21).then_some(get_int_field(
                env,
                interface,
                "getAlternateSetting",
            )? as u8),
            num_endpoints: get_int_field(env, interface, "getEndpointCount")? as u8,
            class: get_int_field(env, interface, "getInterfaceClass")? as u8,
            sub_class: get_int_field(env, interface, "getInterfaceSubclass")? as u8,
            protocol: get_int_field(env, interface, "getInterfaceProtocol")? as u8,
        })
    }

    pub fn get_endpoints(&self) -> Result<Vec<EndpointInfo>, Error> {
        with_jni_env(|env| {
            let mut vec_endp = Vec::new();
            for i in 0..self.num_endpoints {
                let endp = jni_call_ret_obj(
                    env,
                    &self.internal,
                    "getEndpoint",
                    "(I)Landroid/hardware/usb/UsbEndpoint;",
                    &[(i as jint).into()],
                )?;
                vec_endp.push(EndpointInfo::build(env, &endp)?);
            }
            Ok(vec_endp)
        })
    }
}

impl std::fmt::Debug for InterfaceInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = f.debug_struct("InterfaceInfo");

        s.field("interface_number", &self.interface_number);
        s.field("alternate_setting", &self.alternate_setting);
        s.field("num_endpoints", &self.num_endpoints);
        s.field("class", &format_args!("0x{:02X}", self.class));
        s.field("sub_class", &format_args!("0x{:02X}", self.sub_class));
        s.field("protocol", &format_args!("0x{:02X}", self.protocol));

        if let Ok(endps) = self.get_endpoints() {
            for endp in endps {
                s.field("Endpoint", &endp);
            }
        }
        s.finish()
    }
}

impl PartialEq for InterfaceInfo {
    fn eq(&self, other: &Self) -> bool {
        let equ = self.interface_number == other.interface_number
            && self.alternate_setting == other.alternate_setting
            && self.num_endpoints == other.num_endpoints
            && self.class == other.class
            && self.sub_class == other.sub_class
            && self.protocol == other.protocol;
        if !equ {
            return false;
        }
        let vec_endp = self.get_endpoints().unwrap_or_default();
        let vec_endp_other = other.get_endpoints().unwrap_or_default();
        vec_endp == vec_endp_other
    }
}

/// Corresponds to `android.hardware.usb.UsbEndpoint`.
#[derive(Clone, CopyGetters)]
#[getset(get_copy = "pub")]
pub struct EndpointInfo {
    #[getset(skip)]
    pub(crate) internal: jni::objects::GlobalRef,

    number: u8,

    /// Equals `bEndpointAddress`.
    address: u8,
    /// Determined by `bEndpointAddress`.
    direction: EndpointDirection,
    /// From `bmAttributes`.
    transfer_type: EndpointType,
    /// Equals `wMaxPacketSize`.
    max_packet_size: u16,
    /// Equals `bInterval`.
    interval: u8,
}

impl EndpointInfo {
    fn build(env: &mut JNIEnv, endp: &JObject<'_>) -> Result<Self, Error> {
        Ok(Self {
            internal: env.new_global_ref(endp).map_err(jerr)?,

            number: get_int_field(env, endp, "getEndpointNumber")? as u8,
            address: get_int_field(env, endp, "getAddress")? as u8,
            direction: get_int_field(env, endp, "getDirection")?.try_into()?,
            transfer_type: get_int_field(env, endp, "getType")?.try_into()?,
            max_packet_size: get_int_field(env, endp, "getMaxPacketSize")? as u16,
            interval: get_int_field(env, endp, "getInterval")? as u8,
        })
    }
}

impl std::fmt::Debug for EndpointInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EndpointInfo")
            .field("number", &self.number)
            .field("address", &format_args!("0x{:02X}", self.address))
            .field("direction", &self.direction)
            .field("transfer_type", &self.transfer_type)
            .field("max_packet_size", &self.max_packet_size)
            .field("interval", &self.interval)
            .finish()
    }
}

impl PartialEq for EndpointInfo {
    fn eq(&self, other: &Self) -> bool {
        self.number == other.number
            && self.address == other.address
            && self.direction == other.direction
            && self.transfer_type == other.transfer_type
            && self.max_packet_size == other.max_packet_size
            && self.interval == other.interval
    }
}

/// IN our OUT.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum EndpointDirection {
    In,
    Out,
}
impl TryFrom<jint> for EndpointDirection {
    type Error = Error;
    fn try_from(value: jint) -> Result<Self, Self::Error> {
        match value {
            0x00000080 => Ok(EndpointDirection::In),
            0x00000000 => Ok(EndpointDirection::Out),
            _ => Err(Error::Other("bad raw direction value".to_string())),
        }
    }
}
/// Transfer types.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum EndpointType {
    Control,
    Isochronous,
    Bulk,
    Interrupt,
}
impl TryFrom<jint> for EndpointType {
    type Error = Error;
    fn try_from(value: jint) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(EndpointType::Control),
            1 => Ok(EndpointType::Isochronous),
            2 => Ok(EndpointType::Bulk),
            3 => Ok(EndpointType::Interrupt),
            _ => Err(Error::Other("bad raw endp type value".to_string())),
        }
    }
}

/// Types of control transfers.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum CtrlRequestType {
    /// Requests that are defined by the USB standard.
    Standard,
    /// Requests that are defined by a device class, e.g., HID.
    Class,
    /// Vendor-specific requests.
    Vendor,
    /// Reserved for future use.
    Reserved,
}

/// Recipients of control transfers.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum CtrlRecipient {
    /// The recipient is a device.
    Device,
    /// The recipient is an interface.
    Interface,
    /// The recipient is an endpoint.
    Endpoint,
    /// Other.
    Other,
}

// Based on `rusb/src/fields.rs`.

use libusb_constants::*;
mod libusb_constants {
    pub const LIBUSB_ENDPOINT_DIR_MASK: u8 = 0x80;
    pub const LIBUSB_ENDPOINT_IN: u8 = 0x80;
    pub const LIBUSB_ENDPOINT_OUT: u8 = 0x00;

    pub const LIBUSB_REQUEST_TYPE_STANDARD: u8 = 0x00 << 5;
    pub const LIBUSB_REQUEST_TYPE_CLASS: u8 = 0x01 << 5;
    pub const LIBUSB_REQUEST_TYPE_VENDOR: u8 = 0x02 << 5;
    pub const LIBUSB_REQUEST_TYPE_RESERVED: u8 = 0x03 << 5;

    pub const LIBUSB_RECIPIENT_DEVICE: u8 = 0x00;
    pub const LIBUSB_RECIPIENT_INTERFACE: u8 = 0x01;
    pub const LIBUSB_RECIPIENT_ENDPOINT: u8 = 0x02;
    pub const LIBUSB_RECIPIENT_OTHER: u8 = 0x03;
}

/// Builds a valid `request_type` parameter for USB control transfers.
pub const fn ctrl_request_type(
    direction: EndpointDirection,
    request_type: CtrlRequestType,
    recipient: CtrlRecipient,
) -> u8 {
    let mut value: u8 = match direction {
        EndpointDirection::Out => LIBUSB_ENDPOINT_OUT,
        EndpointDirection::In => LIBUSB_ENDPOINT_IN,
    };
    value |= match request_type {
        CtrlRequestType::Standard => LIBUSB_REQUEST_TYPE_STANDARD,
        CtrlRequestType::Class => LIBUSB_REQUEST_TYPE_CLASS,
        CtrlRequestType::Vendor => LIBUSB_REQUEST_TYPE_VENDOR,
        CtrlRequestType::Reserved => LIBUSB_REQUEST_TYPE_RESERVED,
    };
    value |= match recipient {
        CtrlRecipient::Device => LIBUSB_RECIPIENT_DEVICE,
        CtrlRecipient::Interface => LIBUSB_RECIPIENT_INTERFACE,
        CtrlRecipient::Endpoint => LIBUSB_RECIPIENT_ENDPOINT,
        CtrlRecipient::Other => LIBUSB_RECIPIENT_OTHER,
    };
    value
}

pub(crate) const fn ctrl_request_dir(req_type: u8) -> Option<EndpointDirection> {
    match req_type & LIBUSB_ENDPOINT_DIR_MASK {
        LIBUSB_ENDPOINT_IN => Some(EndpointDirection::In),
        LIBUSB_ENDPOINT_OUT => Some(EndpointDirection::Out),
        _ => None,
    }
}

// These functions call java functions without parameter. Error::Other on failure.
#[inline(always)]
fn get_int_field(env: &mut JNIEnv, dev: &JObject<'_>, method: &str) -> Result<jint, Error> {
    env.call_method(dev, method, "()I", &[])
        .and_then(|v| v.i())
        .map_err(jerr)
}
#[inline(always)]
fn get_bool_field(env: &mut JNIEnv, dev: &JObject<'_>, method: &str) -> Result<bool, Error> {
    env.call_method(dev, method, "()Z", &[])
        .and_then(|v| v.z())
        .map_err(jerr)
}
#[inline(always)]
fn get_string_field(env: &mut JNIEnv, dev: &JObject<'_>, method: &str) -> Result<String, Error> {
    let res = env
        .call_method(dev, method, "()Ljava/lang/String;", &[])
        .and_then(|o| o.l())
        .map_err(jerr)?;
    let jstring = JString::from(res);
    let jstr = unsafe { env.get_string_unchecked(&jstring) }.map_err(jerr)?;
    let result = jstr.into();
    drop(env.auto_local(jstring));
    Ok(result)
}
