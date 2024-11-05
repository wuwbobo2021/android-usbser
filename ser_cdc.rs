use std::{
    cell::Cell,
    io::{self, Error, ErrorKind, Read, Write},
    str::FromStr,
    time::Duration,
};

use crate::usb::{
    self, ctrl_request_type, CtrlRecipient, CtrlRequestType, DeviceHandle, DeviceInfo,
    EndpointDirection, EndpointInfo, EndpointType, InterfaceInfo,
};

const USB_INTR_CLASS_COMM: u8 = 0x02;
const USB_INTR_SUBCLASS_ACM: u8 = 0x02;
const USB_INTR_CLASS_CDC_DATA: u8 = 0x0A;

const SET_LINE_CODING: u8 = 0x20;
const SET_CONTROL_LINE_STATE: u8 = 0x22;
const SEND_BREAK: u8 = 0x23;

// send ACM control message
const USB_CTRL_REQTYPE_ACM: u8 = ctrl_request_type(
    EndpointDirection::Out,
    CtrlRequestType::Class,
    CtrlRecipient::Interface,
);

/// This is currently a thin wrapper of USB operations, it requires hardware buffers
/// at the device side. It uses the CDC ACM Data Interface Class to transfer data
/// (the Communication Interface Class is used for probing and serial configuration).
///
/// `serialport::SerialPort` implementation can be enabled by an optional feature.
///
/// Inspired by <https://github.com/mik3y/usb-serial-for-android>, `CdcAcmSerialDriver.java`.
///
/// Reference: *USB Class Definitions for Communication Devices, Version 1.1*,
/// especially section 3.6.2.1, 5.2.3.2 and 6.2(.13).
#[derive(Debug)]
pub struct CdcSerial {
    handle: Option<DeviceHandle>, // always Some before dropping
    ctrl_index: u16,              // communication interface id as the control transfer index
    intr_comm: InterfaceInfo,     // communication interface info, used on dropping
    intr_data: InterfaceInfo,     // data interface info, used on dropping
    endp_read: EndpointInfo,      // bulk IN endpoint of data interface
    endp_write: EndpointInfo,     // bulk OUT endpoint of data interface

    is_connected: Cell<bool>, // Turns from true to false when disconnection is discovered
    timeout: Duration,        // `Read` and `Write` timeout
    ser_conf: Option<SerialConfig>, // keeps the latest settings
    dtr_rts: (bool, bool),    // keeps the latest settings, (false, false) by default
}

impl CdcSerial {
    /// Probes for CDC-ACM devices. It checks the current configuration of each device.
    /// Returns an empty vector if no device is found.
    pub fn probe() -> io::Result<Vec<DeviceInfo>> {
        let vec_dev = usb::list_devices().map_err(|e| {
            if let usb::Error::NotSupported = e {
                // check it for once because `probe()` is probably called at first
                Error::new(ErrorKind::Unsupported, "cannot get USB_SERVICE")
            } else {
                Error::other(e)
            }
        })?;
        let mut vec_found = Vec::new();
        for dev in vec_dev {
            if Self::cdc_acm_intrs(&dev).is_some() {
                vec_found.push(dev);
            }
        }
        Ok(vec_found)
    }

    /// Connects to the CDC-ACM device, returns the `CdcSerial` handler.
    /// Please get permission for the device before calling this function.
    /// - `timeout`: Set for standard `Read` and `Write` traits.
    ///   Note: zero `timeout` duration indicates infinite timeout.
    pub fn build(dev_info: &DeviceInfo, timeout: Duration) -> io::Result<Self> {
        let (intr_comm, intr_data) = Self::cdc_acm_intrs(dev_info)
            .ok_or(Error::new(ErrorKind::InvalidInput, "not a CDC-ACM device"))?;
        let ctrl_index = intr_comm.interface_number() as u16;
        let (mut endp_r, mut endp_w) = (None, None);
        for endp in intr_data
            .get_endpoints()
            .map_err(Error::other)?
            .into_iter()
            .filter(|endp| endp.transfer_type() == EndpointType::Bulk)
        {
            if endp.direction() == EndpointDirection::In {
                let _ = endp_r.get_or_insert(endp);
            } else {
                let _ = endp_w.get_or_insert(endp);
            }
        }
        let (endp_read, endp_write) = if let (Some(r), Some(w)) = (endp_r, endp_w) {
            (r, w)
        } else {
            return Err(Error::new(ErrorKind::NotFound, "data endpoints not found"));
        };

        let handle = dev_info
            .open_device()
            .map_err(|e| Error::new(ErrorKind::PermissionDenied, e))?;
        // data transfer might be done without calling `claim_interface()`
        let _ = handle.claim_interface(&intr_comm, true);
        let _ = handle.claim_interface(&intr_data, true);

        Ok(Self {
            handle: Some(handle),
            ctrl_index,
            intr_comm,
            intr_data,
            endp_read,
            endp_write,
            is_connected: Cell::new(true),
            timeout,
            ser_conf: None,
            dtr_rts: (false, false),
        })
    }

    /// Returns (intr_comm, intr_data) if it is a CDC-ACM device.
    fn cdc_acm_intrs(dev_info: &DeviceInfo) -> Option<(InterfaceInfo, InterfaceInfo)> {
        let (mut intr_comm, mut intr_data) = (None, None);
        let vec_intr = dev_info.get_interfaces().ok()?;
        for intr in vec_intr {
            if intr.class() == USB_INTR_CLASS_COMM && intr.sub_class() == USB_INTR_SUBCLASS_ACM {
                let _ = intr_comm.get_or_insert(intr);
            } else if intr.class() == USB_INTR_CLASS_CDC_DATA {
                let _ = intr_data.get_or_insert(intr);
            }
        }
        if let (Some(intr_comm), Some(intr_data)) = (intr_comm, intr_data) {
            Some((intr_comm, intr_data))
        } else {
            None
        }
    }

    /// Sets timeout for standard `Read` and `Write` implementations to do USB
    /// bulk transfers. Note: zero `timeout` parameter indicates infinite timeout.
    #[cfg(not(feature = "serialport"))]
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Applies serial parameters.
    pub fn set_config(&mut self, conf: SerialConfig) -> io::Result<()> {
        let conf_bytes: [u8; 7] = conf.into();
        self.cdc_acm_ctrl_set(SET_LINE_CODING, 0, &conf_bytes)?;
        self.ser_conf.replace(conf);
        Ok(())
    }

    /// Sets DTR and RTS states.
    pub fn set_dtr_rts(&mut self, dtr: bool, rts: bool) -> io::Result<()> {
        let val_dtr = if dtr { 0x1 } else { 0x0 };
        let val_rts = if rts { 0x2 } else { 0x0 };
        let val = (val_dtr | val_rts) as u16;
        self.cdc_acm_ctrl_set(SET_CONTROL_LINE_STATE, val, &[])?;
        self.dtr_rts = (dtr, rts);
        Ok(())
    }

    /// Sets the break state.
    pub fn set_break_state(&self, val: bool) -> io::Result<()> {
        let val = if val { 0xffff } else { 0 } as u16;
        self.cdc_acm_ctrl_set(SEND_BREAK, val, &[])
    }

    fn cdc_acm_ctrl_set(&self, request: u8, value: u16, buf: &[u8]) -> io::Result<()> {
        let sz_write = self
            .get_handle()?
            .write_control(
                USB_CTRL_REQTYPE_ACM,
                request,
                value,
                self.ctrl_index,
                buf,
                self.timeout * 2,
            )
            .map_err(|e| self.err_map_to_io(e))?;
        if sz_write == buf.len() {
            Ok(())
        } else {
            Err(Error::new(
                ErrorKind::Interrupted,
                "cdc_acm_ctrl_set(), wrong written size",
            ))
        }
    }

    fn get_handle(&self) -> io::Result<&DeviceHandle> {
        if self.is_connected.get() {
            Ok(self.handle.as_ref().unwrap())
        } else {
            Err(Error::new(
                ErrorKind::NotConnected,
                "the device has been disconnected",
            ))
        }
    }

    /// Checks for connection on USB error, sets the mark if disconnected.
    /// TODO: do more precise mapping (is it possible?)
    fn err_map_to_io(&self, err: usb::Error) -> Error {
        if self.is_connected.get() && self.handle.as_ref().unwrap().check_connection() {
            Error::new(ErrorKind::Other, err)
        } else {
            self.is_connected.set(false);
            Error::new(ErrorKind::NotConnected, err)
        }
    }
}

impl Read for CdcSerial {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.get_handle()?
            .read_bulk(&self.endp_read, buf, self.timeout)
            .map_err(|e| self.err_map_to_io(e))
    }
}

impl Write for CdcSerial {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.get_handle()?
            .write_bulk(&self.endp_write, buf, self.timeout)
            .map_err(|e| self.err_map_to_io(e))
    }

    fn flush(&mut self) -> io::Result<()> {
        // read_bulk() and write_bulk() are synchronous functions.
        Ok(())
    }
}

impl Drop for CdcSerial {
    fn drop(&mut self) {
        let handle = self.handle.take().unwrap();
        let _ = handle.release_interface(&self.intr_comm);
        let _ = handle.release_interface(&self.intr_data);
        handle.close();
    }
}

/// Sets baudrate, parity check mode, data bits and stop bits.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct SerialConfig {
    pub baud_rate: u32,
    pub parity: Parity,
    pub data_bits: DataBits,
    pub stop_bits: StopBits,
}

impl Default for SerialConfig {
    fn default() -> Self {
        Self {
            baud_rate: 9600,
            parity: Parity::None,
            data_bits: DataBits::Eight,
            stop_bits: StopBits::One,
        }
    }
}

impl FromStr for SerialConfig {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad_par = ErrorKind::InvalidInput;
        let mut strs = s.split(',');

        let str_baud = strs.next().ok_or(Error::new(bad_par, s))?;
        let baud_rate = str_baud
            .trim()
            .parse()
            .map_err(|_| Error::new(bad_par, s))?;

        let str_parity = strs.next().ok_or(Error::new(bad_par, s))?;
        let parity = match str_parity
            .trim()
            .chars()
            .next()
            .ok_or(Error::new(bad_par, s))?
        {
            'N' => Parity::None,
            'O' => Parity::Odd,
            'E' => Parity::Even,
            'M' => Parity::Mark,
            'S' => Parity::Space,
            _ => return Err(Error::new(bad_par, s)),
        };

        let str_data_bits = strs.next().ok_or(Error::new(bad_par, s))?;
        let data_bits = str_data_bits
            .trim()
            .parse()
            .map_err(|_| Error::new(bad_par, s))?;
        let data_bits = match data_bits {
            5 => DataBits::Five,
            6 => DataBits::Six,
            7 => DataBits::Seven,
            8 => DataBits::Eight,
            16 => DataBits::Sixteen,
            _ => return Err(Error::new(bad_par, s)),
        };

        let str_stop_bits = strs.next().ok_or(Error::new(bad_par, s))?;
        let stop_bits = str_stop_bits
            .trim()
            .parse()
            .map_err(|_| Error::new(bad_par, s))?;
        let stop_bits = match stop_bits {
            1. => StopBits::One,
            1.5 => StopBits::OnePointFive,
            2. => StopBits::Two,
            _ => return Err(Error::new(bad_par, s)),
        };

        Ok(Self {
            baud_rate,
            parity,
            data_bits,
            stop_bits,
        })
    }
}

impl std::fmt::Display for SerialConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let baud_rate = self.baud_rate;
        let parity = match self.parity {
            Parity::None => 'N',
            Parity::Odd => 'O',
            Parity::Even => 'E',
            Parity::Mark => 'M',
            Parity::Space => 'S',
        };
        let data_bits = match self.data_bits {
            DataBits::Five => "5",
            DataBits::Six => "6",
            DataBits::Seven => "7",
            DataBits::Eight => "8",
            DataBits::Sixteen => "16",
        };
        let stop_bits = match self.stop_bits {
            StopBits::One => "1",
            StopBits::OnePointFive => "1.5",
            StopBits::Two => "2",
        };
        write!(f, "{baud_rate},{parity},{data_bits},{stop_bits}")
    }
}

#[allow(clippy::from_over_into)]
impl Into<[u8; 7]> for SerialConfig {
    fn into(self) -> [u8; 7] {
        let mut bytes = [0u8; 7];
        bytes[..4].copy_from_slice(&self.baud_rate.to_le_bytes());
        bytes[4] = match self.stop_bits {
            StopBits::One => 0u8,
            StopBits::OnePointFive => 1u8,
            StopBits::Two => 2u8,
        };
        bytes[5] = match self.parity {
            Parity::None => 0u8,
            Parity::Odd => 1u8,
            Parity::Even => 2u8,
            Parity::Mark => 3u8,
            Parity::Space => 4u8,
        };
        bytes[6] = match self.data_bits {
            DataBits::Five => 5,
            DataBits::Six => 6,
            DataBits::Seven => 7,
            DataBits::Eight => 8,
            DataBits::Sixteen => 16,
        };
        bytes
    }
}

/// Number of bits per character.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum DataBits {
    /// 5 bits per character.
    Five,
    /// 6 bits per character.
    Six,
    /// 7 bits per character.
    Seven,
    /// 8 bits per character.
    Eight,
    /// 16 bits per character.
    Sixteen,
}

/// Parity checking modes.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Parity {
    /// No parity bit.
    None,
    /// Parity bit sets odd number of 1 bits.
    Odd,
    /// Parity bit sets even number of 1 bits.
    Even,
    /// Leaves the parity bit set to 1.
    Mark,
    /// Leaves the parity bit set to 0.
    Space,
}

/// Stop bits are transmitted after every character.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum StopBits {
    /// One stop bit.
    One,
    /// 1.5 stop bits.
    OnePointFive,
    /// Two stop bits.
    Two,
}

#[cfg(feature = "serialport")]
mod impl_serialport {
    use super::*;

    impl From<serialport::DataBits> for DataBits {
        fn from(value: serialport::DataBits) -> Self {
            match value {
                serialport::DataBits::Five => DataBits::Five,
                serialport::DataBits::Six => DataBits::Six,
                serialport::DataBits::Seven => DataBits::Seven,
                serialport::DataBits::Eight => DataBits::Eight,
            }
        }
    }

    impl TryFrom<DataBits> for serialport::DataBits {
        type Error = serialport::Error;
        fn try_from(value: DataBits) -> Result<Self, Self::Error> {
            let value = match value {
                DataBits::Five => serialport::DataBits::Five,
                DataBits::Six => serialport::DataBits::Six,
                DataBits::Seven => serialport::DataBits::Seven,
                DataBits::Eight => serialport::DataBits::Eight,
                DataBits::Sixteen => {
                    return Err(serialport::Error::new(
                        serialport::ErrorKind::Unknown,
                        "DataBits::Sixteen is not supported by crate `serialport`",
                    ));
                }
            };
            Ok(value)
        }
    }

    impl From<serialport::Parity> for Parity {
        fn from(value: serialport::Parity) -> Self {
            match value {
                serialport::Parity::None => Parity::None,
                serialport::Parity::Odd => Parity::Odd,
                serialport::Parity::Even => Parity::Even,
            }
        }
    }

    impl TryFrom<Parity> for serialport::Parity {
        type Error = serialport::Error;
        fn try_from(value: Parity) -> Result<Self, Self::Error> {
            let value = match value {
                Parity::None => serialport::Parity::None,
                Parity::Odd => serialport::Parity::Odd,
                Parity::Even => serialport::Parity::Even,
                _ => {
                    return Err(serialport::Error::new(
                        serialport::ErrorKind::Unknown,
                        "current parity mode is not supported by crate `serialport`",
                    ));
                }
            };
            Ok(value)
        }
    }

    impl From<serialport::StopBits> for StopBits {
        fn from(value: serialport::StopBits) -> Self {
            match value {
                serialport::StopBits::One => StopBits::One,
                serialport::StopBits::Two => StopBits::Two,
            }
        }
    }

    impl TryFrom<StopBits> for serialport::StopBits {
        type Error = serialport::Error;
        fn try_from(value: StopBits) -> Result<Self, Self::Error> {
            let value = match value {
                StopBits::One => serialport::StopBits::One,
                StopBits::Two => serialport::StopBits::Two,
                StopBits::OnePointFive => {
                    return Err(serialport::Error::new(
                        serialport::ErrorKind::Unknown,
                        "1.5 stop bits is not supported by crate `serialport`",
                    ));
                }
            };
            Ok(value)
        }
    }

    #[inline(always)]
    fn err_map_to_serialport(err: Error) -> serialport::Error {
        let desc = err.to_string();
        let kind = match err.kind() {
            ErrorKind::NotConnected => serialport::ErrorKind::NoDevice,
            ErrorKind::InvalidInput => serialport::ErrorKind::InvalidInput,
            _ => serialport::ErrorKind::Io(err.kind()),
        };
        serialport::Error::new(kind, desc)
    }

    fn err_unsupported_op() -> serialport::Error {
        err_map_to_serialport(Error::new(
            ErrorKind::Unsupported,
            "unsupported function in trait `Serialport`",
        ))
    }

    impl CdcSerial {
        fn get_conf_for_serialport(&self) -> Result<&SerialConfig, serialport::Error> {
            self.ser_conf.as_ref().ok_or(serialport::Error::new(
                serialport::ErrorKind::Io(std::io::ErrorKind::NotFound),
                "serial configuration haven't been set",
            ))
        }
    }

    impl serialport::SerialPort for CdcSerial {
        fn name(&self) -> Option<String> {
            self.get_handle()
                .ok()
                .map(|hdl| hdl.device_info().path_name().clone())
        }

        fn baud_rate(&self) -> serialport::Result<u32> {
            Ok(self.get_conf_for_serialport()?.baud_rate)
        }
        fn data_bits(&self) -> serialport::Result<serialport::DataBits> {
            self.get_conf_for_serialport()?.data_bits.try_into()
        }
        fn parity(&self) -> serialport::Result<serialport::Parity> {
            self.get_conf_for_serialport()?.parity.try_into()
        }
        fn stop_bits(&self) -> serialport::Result<serialport::StopBits> {
            self.get_conf_for_serialport()?.stop_bits.try_into()
        }

        fn flow_control(&self) -> serialport::Result<serialport::FlowControl> {
            Ok(serialport::FlowControl::None)
        }

        fn timeout(&self) -> Duration {
            self.timeout
        }

        fn set_baud_rate(&mut self, baud_rate: u32) -> serialport::Result<()> {
            let mut conf = self.ser_conf.unwrap_or_default();
            conf.baud_rate = baud_rate;
            self.set_config(conf).map_err(err_map_to_serialport)
        }

        fn set_data_bits(&mut self, data_bits: serialport::DataBits) -> serialport::Result<()> {
            let mut conf = self.ser_conf.unwrap_or_default();
            conf.data_bits = data_bits.into();
            self.set_config(conf).map_err(err_map_to_serialport)
        }

        fn set_parity(&mut self, parity: serialport::Parity) -> serialport::Result<()> {
            let mut conf = self.ser_conf.unwrap_or_default();
            conf.parity = parity.into();
            self.set_config(conf).map_err(err_map_to_serialport)
        }

        fn set_stop_bits(&mut self, stop_bits: serialport::StopBits) -> serialport::Result<()> {
            let mut conf = self.ser_conf.unwrap_or_default();
            conf.stop_bits = stop_bits.into();
            self.set_config(conf).map_err(err_map_to_serialport)
        }

        fn set_flow_control(
            &mut self,
            _flow_control: serialport::FlowControl,
        ) -> serialport::Result<()> {
            Err(err_unsupported_op())
        }

        /// Sets timeout for standard `Read` and `Write` implementations to do USB
        /// bulk transfers. Note: zero `timeout` parameter indicates infinite timeout.
        fn set_timeout(&mut self, timeout: Duration) -> serialport::Result<()> {
            self.timeout = timeout;
            Ok(())
        }

        #[inline(always)]
        fn write_request_to_send(&mut self, value: bool) -> serialport::Result<()> {
            let (dtr, _) = self.dtr_rts;
            let rts = value;
            self.set_dtr_rts(dtr, rts).map_err(err_map_to_serialport)
        }

        #[inline(always)]
        fn write_data_terminal_ready(&mut self, value: bool) -> serialport::Result<()> {
            let (_, rts) = self.dtr_rts;
            let dtr = value;
            self.set_dtr_rts(dtr, rts).map_err(err_map_to_serialport)
        }

        /// Unsupported.
        fn read_clear_to_send(&mut self) -> serialport::Result<bool> {
            Err(err_unsupported_op())
        }
        /// Unsupported.
        fn read_data_set_ready(&mut self) -> serialport::Result<bool> {
            Err(err_unsupported_op())
        }
        /// Unsupported.
        fn read_ring_indicator(&mut self) -> serialport::Result<bool> {
            Err(err_unsupported_op())
        }
        /// Unsupported.
        fn read_carrier_detect(&mut self) -> serialport::Result<bool> {
            Err(err_unsupported_op())
        }

        /// Returns 0 because no buffer is maintained here, and all operations are synchronous.
        fn bytes_to_read(&self) -> serialport::Result<u32> {
            Ok(0)
        }
        /// Returns 0 because no buffer is maintained here, and all operations are synchronous.
        fn bytes_to_write(&self) -> serialport::Result<u32> {
            Ok(0)
        }
        /// Does nothing.
        fn clear(&self, _buffer_to_clear: serialport::ClearBuffer) -> serialport::Result<()> {
            Ok(())
        }

        #[inline(always)]
        fn set_break(&self) -> serialport::Result<()> {
            self.set_break_state(true).map_err(err_map_to_serialport)
        }
        #[inline(always)]
        fn clear_break(&self) -> serialport::Result<()> {
            self.set_break_state(false).map_err(err_map_to_serialport)
        }

        /// Unsupported.
        fn try_clone(&self) -> serialport::Result<Box<dyn serialport::SerialPort>> {
            Err(err_unsupported_op())
        }
    }
}
