//! Android USB serial driver, currently works with CDC-ACM devices.
//!
//! Inspired by <https://github.com/mik3y/usb-serial-for-android>.
//!
//! It is far from being feature-complete. Of course you can make use of something like
//! [react-native-usb-serialport](https://www.npmjs.com/package/react-native-usb-serialport),
//! however, that may introduce multiple layers between Rust and the Linux kernel.
//!
//! This crate uses `ndk_context::AndroidContext`, usually initialized by `android_activity`.
//!
//! The initial version of this crate performs USB transfers through JNI calls but not `nusb`,
//! do not use it except you have encountered compatibility problems.

mod ser_cdc;
mod usb_conn;
mod usb_info;
mod usb_sync;
pub use ser_cdc::*;

/// Equals `std::io::Error`.
pub type Error = std::io::Error;

/// Android helper for `nusb`. It may be merged into that crate in the future.
///
/// Reference:
/// - <https://developer.android.com/develop/connectivity/usb/host>
/// - <https://developer.android.com/reference/android/hardware/usb/package-summary>
pub mod usb {
    pub use crate::usb_conn::*;
    pub use crate::usb_info::*;
    pub use crate::usb_sync::*;
    pub use crate::Error;

    /// Maps unexpected JNI errors to `std::io::Error` of `ErrorKind::Other`
    /// (`From<jni::errors::Error>` cannot be implemented for `std::io::Error`
    /// here because of the orphan rule). Side effect: `jni_last_cleared_ex()`.
    #[inline(always)]
    pub(crate) fn jerr(err: jni_min_helper::jni::errors::Error) -> Error {
        use jni::errors::Error::*;
        use jni_min_helper::*;
        if let JavaException = err {
            let err = jni_clear_ex(err);
            jni_last_cleared_ex()
                .ok_or(JavaException)
                .and_then(|ex| Ok((ex, jni_attach_vm()?)))
                .and_then(|(ex, ref mut env)| {
                    Ok((ex.get_class_name(env)?, ex.get_throwable_msg(env)?))
                })
                .map(|(cls, msg)| Error::other(format!("{cls}: {msg}")))
                .unwrap_or(Error::other(err))
        } else {
            Error::other(err)
        }
    }
}

use serialport::{DataBits, Parity, StopBits};

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

impl std::str::FromStr for SerialConfig {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad_par = std::io::ErrorKind::InvalidInput;
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
            _ => return Err(Error::new(bad_par, s)),
        };

        let str_stop_bits = strs.next().ok_or(Error::new(bad_par, s))?;
        let stop_bits = str_stop_bits
            .trim()
            .parse()
            .map_err(|_| Error::new(bad_par, s))?;
        let stop_bits = match stop_bits {
            1. => StopBits::One,
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
        };
        let data_bits = match self.data_bits {
            DataBits::Five => "5",
            DataBits::Six => "6",
            DataBits::Seven => "7",
            DataBits::Eight => "8",
        };
        let stop_bits = match self.stop_bits {
            StopBits::One => "1",
            StopBits::Two => "2",
        };
        write!(f, "{baud_rate},{parity},{data_bits},{stop_bits}")
    }
}
