# android-usbser-rs

Android host driver for USB serial adapters, currently works with CDC-ACM devices.

Currently it is far from being feature-complete, and bug reports will be helpful.

Documentation: <https://docs.rs/android_usbser/latest>.

TODO: Implement drivers for other serial adapters, for example, those with FTDI, Prolific, or CH34x chips.

## Testing

Make sure the JDK, Android SDK and NDK is installed and configured, and the Rust target `aarch64-linux-android` is installed. Install `cargo-apk` and make sure the release keystore is configured.

Create a folder named `android-usb-cdc-test`, and create `Cargo.toml` inside it:

```toml
[package]
name = "android-usb-cdc-test"
version = "0.1.0"
edition = "2021"
publish = false

[dependencies]
log = "0.4"
android_logger = "0.14"
android-activity = { version = "0.6", features = ["native-activity"] }
android-usbser = "0.2.1"
serialport = "4.6"

[lib]
name = "android_usb_cdc_test"
crate-type = ["cdylib"]
path = "lib.rs"

[package.metadata.android]
package = "com.example.android_usb_cdc_test"
build_targets = [ "aarch64-linux-android" ]
resources = "./res"

[package.metadata.android.sdk]
min_sdk_version = 16
target_sdk_version = 30

[[package.metadata.android.uses_feature]]
name = "android.hardware.usb.host"
required = true

[[package.metadata.android.application.activity.intent_filter]]
actions = ["android.hardware.usb.action.USB_DEVICE_ATTACHED"]

# Please check <https://github.com/rust-mobile/cargo-apk/pull/67> if it fails.
# Otherwise comment out the lines below (request for permission purely at runtime).
[[package.metadata.android.application.activity.meta_data]]
name = "android.hardware.usb.action.USB_DEVICE_ATTACHED"
resource = "@xml/device_filter"
```

### USB connection test (polling in the main thread)

`lib.rs`:

```rust
use android_activity::{AndroidApp, MainEvent, PollEvent};
use android_usbser::usb;
use log::{info, warn};
use std::time::Duration;

#[no_mangle]
fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("android_usb_cdc_test"),
    );

    let mut startup_dev = usb::check_attached_intent().ok();
    let mut watcher = usb::watch_devices().unwrap();
    let mut perm_req: Option<usb::PermissionRequest> = None;
    let mut on_destroy = false;
    loop {
        app.poll_events(
            Some(Duration::from_secs(1)), // timeout
            |event| match event {
                PollEvent::Main(MainEvent::Start) => {
                    info!("Main Start.");
                }
                PollEvent::Main(MainEvent::Resume { loader: _, .. }) => {
                    info!("Main Resume.");
                }
                PollEvent::Main(MainEvent::Pause) => {
                    info!("Main Pause.");
                }
                PollEvent::Main(MainEvent::Stop) => {
                    info!("Main Stop.");
                }
                PollEvent::Main(MainEvent::Destroy) => {
                    info!("Main Destroy.");
                    on_destroy = true;
                }
                _ => (),
            },
        );
        if on_destroy {
            return;
        }
        let dev_info = if let Some(dev) = startup_dev.take() {
            info!("Got device from startup intent.");
            if !dev.has_permission().unwrap() {
                warn!("Unexpected: no permission.");
                continue;
            }
            dev
        } else if let Some(req) = perm_req.as_ref() {
            if !req.responsed() {
                continue;
            }
            let req = perm_req.take().unwrap();
            let dev = req.device_info().clone();
            // `responsed()` returned true, so unwrap here.
            if !req.take_response().unwrap() {
                // boolean response
                info!("Permission denied.");
                continue;
            }
            dev
        } else {
            match watcher.take_next() {
                None => continue,
                Some(usb::HotplugEvent::Connected(dev)) => {
                    info!("Device connected ({}).", dev.path_name());
                    if dev.has_permission().unwrap() {
                        dev
                    } else {
                        perm_req = dev.request_permission().unwrap_or(None);
                        if perm_req.is_some() {
                            info!("Performing permission request...");
                        }
                        continue;
                    }
                }
                Some(usb::HotplugEvent::Disconnected(dev)) => {
                    info!("Device disconnected ({}).", dev.path_name());
                    continue;
                }
            }
        };

        let Ok(conn) = dev_info.open_device() else {
            warn!("Unexpected: failed to open the device.");
            continue;
        };
        info!("Opened, printing USB configurations:");
        for conf in conn.configurations() {
            info!("{:#?}", conf);
        }
        std::thread::sleep(Duration::from_secs(1));
        info!("Closing the device.");
    }
}
```

`res/xml/device_filter.xml`:

```xml
<?xml version="1.0" encoding="utf-8"?>

<resources>
    <usb-device class="2" />
</resources>
```

Note: `class="2"` (Communications Class) can be deleted if you want it to match any device in this test.

Run `cargo apk build -r`. Connect the Android phone to the PC via USB, then configure the adbd TCP port and connect to it. Install the APK package with `adb install`.

Run `adb logcat android_usb_cdc_test:D '*:S'` On PC for tracing, then start the installed Android "App" (`android_usb_cdc_test`). Try to connect and disconnect some USB devices.

### USB connection test (background thread)

```rust
use android_activity::{AndroidApp, MainEvent, PollEvent};
use android_usbser::usb;
use log::{info, warn};
use std::{sync::Mutex, time::Duration};

#[no_mangle]
fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("android_usb_cdc_test"),
    );

    let mut on_destroy = false;
    loop {
        app.poll_events(
            Some(std::time::Duration::from_secs(1)), // timeout
            |event| match event {
                PollEvent::Main(MainEvent::Start) => {
                    info!("Main Start.");
                    start_background_thread();
                }
                PollEvent::Main(MainEvent::Stop) => {
                    info!("Main Stop.");
                    stop_background_thread();
                }
                PollEvent::Main(MainEvent::Destroy) => {
                    info!("Main Destroy.");
                    on_destroy = true;
                }
                _ => (),
            },
        );
        if on_destroy {
            return;
        }
    }
}

static BACKGROUND_THREAD: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);
static FLAG_EXIT: Mutex<bool> = Mutex::new(false);

fn start_background_thread() {
    let mut th_hdl = BACKGROUND_THREAD.lock().unwrap();
    if th_hdl.is_none() {
        th_hdl.replace(std::thread::spawn(background_loop));
        info!("Background thread started.");
    }
}

fn stop_background_thread() {
    let mut th_hdl = BACKGROUND_THREAD.lock().unwrap();
    if let Some(th_hdl) = th_hdl.take() {
        *FLAG_EXIT.lock().unwrap() = true;
        if th_hdl.join().is_ok() {
            info!("Background thread stopped normally.");
        } else {
            info!("Failed to join the background thread.");
        };
        *FLAG_EXIT.lock().unwrap() = false;
    }
}

#[inline(always)]
fn check_flag_exit() -> bool {
    *FLAG_EXIT.lock().unwrap()
}

fn background_loop() {
    let mut watcher = usb::watch_devices().unwrap();
    let mut startup_dev = usb::check_attached_intent().ok();
    loop {
        let dev = if let Some(dev) = startup_dev.take() {
            info!("Got device from startup intent.");
            dev
        } else {
            match watcher.wait_blocking(Duration::from_millis(500)) {
                None => continue,
                Some(usb::HotplugEvent::Connected(dev)) => {
                    info!("Device connected ({}).", dev.path_name());
                    dev
                }
                Some(usb::HotplugEvent::Disconnected(dev)) => {
                    info!("Device disconnected ({}).", dev.path_name());
                    continue;
                }
            }
        };
        info!("{:#?}", dev);
        let granted = if let Some(req) = dev.request_permission().unwrap() {
            info!("Performing permission request...");
            let result = req.wait_blocking(Duration::from_secs(10));
            info!("Request result: {:?}", result);
            result.unwrap_or(dev.has_permission().unwrap())
        } else {
            info!("Permission is already granted.");
            true
        };
        if granted {
            let Ok(conn) = dev.open_device() else {
                warn!("Unexpected: failed to open the device.");
                continue;
            };
            info!("Opened, printing USB configurations:");
            for conf in conn.configurations() {
                info!("{:#?}", conf);
            }
            std::thread::sleep(Duration::from_secs(1));
            info!("Closing the device.");
            drop(conn);
        }
        if check_flag_exit() {
            return;
        }
    }
}
```

### Serial test

```rust
// This is merely a simplest test program for the library crate.
// To build a regular application, please use some UI framework like Slint (or Tauri?).

use android_activity::{AndroidApp, MainEvent, PollEvent};
use android_usbser::{usb, CdcSerial, SerialConfig};
use log::{info, warn};
use serialport::SerialPort;
use std::{
    io::{self, BufRead, BufReader, Write},
    str::FromStr,
    sync::Mutex,
    time::{Duration, SystemTime},
};

#[no_mangle]
fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("android_usb_cdc_test"),
    );

    let usb_devs = usb::list_devices().unwrap();
    info!("Connected USB devices found on startup:");
    info!("{:#?}", usb_devs);

    let mut on_destroy = false;
    loop {
        app.poll_events(
            Some(std::time::Duration::from_secs(1)), // timeout
            |event| match event {
                PollEvent::Main(MainEvent::Start) => {
                    info!("Main Start.");
                    start_serial_thread();
                }
                PollEvent::Main(MainEvent::Stop) => {
                    info!("Main Stop.");
                    stop_serial_thread();
                }
                PollEvent::Main(MainEvent::Destroy) => {
                    info!("Main Destroy.");
                    on_destroy = true;
                }
                _ => (),
            },
        );
        if on_destroy {
            return;
        }
    }
}

static SERIAL_THREAD: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);
static FLAG_EXIT: Mutex<bool> = Mutex::new(false);

fn start_serial_thread() {
    let mut th_hdl = SERIAL_THREAD.lock().unwrap();
    if th_hdl.is_none() {
        th_hdl.replace(std::thread::spawn(serial_probe_loop));
        info!("Serial thread started.");
    }
}

fn stop_serial_thread() {
    let mut th_hdl = SERIAL_THREAD.lock().unwrap();
    if let Some(th_hdl) = th_hdl.take() {
        *FLAG_EXIT.lock().unwrap() = true;
        if th_hdl.join().is_ok() {
            info!("Serial thread stopped normally.");
        } else {
            warn!("Failed to join serial thread.");
        };
        *FLAG_EXIT.lock().unwrap() = false;
    }
}

// Functions below are executed in the serial thread.

#[inline(always)]
fn check_flag_exit() -> bool {
    *FLAG_EXIT.lock().unwrap()
}

fn thread_delay_ms(ms: u64) -> bool {
    let t_break = SystemTime::now() + Duration::from_millis(ms);
    while SystemTime::now() < t_break {
        if check_flag_exit() {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}

fn serial_probe_loop() {
    let mut startup_dev = usb::check_attached_intent().ok();
    loop {
        let usb_cdc_dev = if let Some(dev) = startup_dev.take() {
            info!("Got device from startup intent.");
            dev
        } else {
            let usb_cdc_devs = CdcSerial::probe().unwrap();
            if usb_cdc_devs.is_empty() {
                info!("No CDC serial adapter found.");
                if !thread_delay_ms(1000) {
                    return;
                }
                continue;
            }
            usb_cdc_devs.into_iter().next().unwrap()
        };

        info!("Opening {} ...", usb_cdc_dev.path_name());

        if let Some(perm_req) = usb_cdc_dev.request_permission().unwrap() {
            for _ in 0..10 {
                if perm_req.responsed() {
                    break;
                }
                if !thread_delay_ms(1000) {
                    return;
                }
                info!("Waiting...");
            }
        }
        if !usb_cdc_dev.has_permission().unwrap() {
            info!("Permission not granted.");
            continue;
        }
        info!("Got permission.");

        let mut serial = CdcSerial::build(&usb_cdc_dev, Duration::from_millis(300)).unwrap();
        let initial_conf = "115200,N,8,1".parse().unwrap();
        info!("Opened, setting {initial_conf} ...");
        serial.set_config(initial_conf).unwrap();
        info!("Configuration set.");

        serial_conn_loop(serial);

        if !thread_delay_ms(1000) {
            return;
        }
    }
}

fn serial_conn_loop(serial: CdcSerial) {
    // BufReader is used here for convenience (and testing), but NOT performance.
    let mut serial_reader = BufReader::new(Box::new(serial) as Box<dyn SerialPort>);

    let mut last_cmd = String::new();
    loop {
        if check_flag_exit() {
            return;
        }

        last_cmd.clear();
        match serial_reader.read_line(&mut last_cmd) {
            Ok(_) => (),
            Err(e) if e.kind() == io::ErrorKind::NotConnected => return,
            _ => continue,
        }

        let mut iter_tokens = last_cmd.split_whitespace();
        let Some(cmd_name) = iter_tokens.next() else {
            continue;
        };

        let mut is_cmd = true;
        match cmd_name {
            "conf" => {
                let conf;
                if let Some(s) = iter_tokens.next() {
                    conf = s.trim();
                } else {
                    warn!("Error: 'conf' without parameter.");
                    continue;
                }
                if let Ok(conf) = SerialConfig::from_str(conf) {
                    let serial = serial_reader.get_mut();
                    if let Err(s) = config_serialport(serial.as_mut(), &conf) {
                        warn!("{s}");
                        continue;
                    }
                } else {
                    warn!("Error: failed to parse '{conf}' into serial parameters.");
                    continue;
                }
            }
            "rts" => {
                let value = match iter_tokens.next().map(|s| s.trim()) {
                    Some("0") | Some("false") => false,
                    Some("1") | Some("true") => true,
                    _ => {
                        warn!("Error: failed to parse RTS value.");
                        continue;
                    }
                };
                if let Err(e) = serial_reader.get_mut().write_request_to_send(value) {
                    warn!("Error: failed to write RTS value: {e}");
                    continue;
                }
            }
            _ => {
                is_cmd = false; // loopback
            }
        }

        let result = if is_cmd {
            serial_reader.get_mut().write_all("Ok\n".as_bytes())
        } else {
            let last_line_upper = last_cmd.to_uppercase();
            serial_reader
                .get_mut()
                .write_all(last_line_upper.as_bytes())
        };
        if let Err(e) = result {
            warn!("Error: failed to response: {e}");
        }
    }
}

// `set_config()` is available in `CdcSerial`, but this is testing `SerialPort` trait impl.
fn config_serialport(serial: &mut dyn SerialPort, conf: &SerialConfig) -> Result<(), String> {
    serial
        .set_baud_rate(conf.baud_rate)
        .map_err(|e| format!("Error: failed to set baudrate: {e}."))?;
    serial
        .set_parity(conf.parity)
        .map_err(|e| format!("Error: failed to set parity: {e}."))?;
    serial
        .set_data_bits(conf.data_bits)
        .map_err(|e| format!("Error: failed to set data bits: {e}."))?;
    serial
        .set_stop_bits(conf.stop_bits)
        .map_err(|e| format!("Error: failed to set stop bits: {e}."))?;

    info!(
        "SerialPort parameters set: {} {} {} {}",
        conf.baud_rate, conf.parity, conf.data_bits, conf.stop_bits
    );
    Ok(())
}
```

After installing the APK package and running `logcat`, connect your USB CDC-ACM serial adapter to the phone, connect GND, Tx and Rx to another serial adapter at the PC side.

On PC, find the PC side serial adapter's name, open the serial terminal tool and set the right port, set `Baudrate` to 115200, make sure `Parity` is None, `Data Bits` is 8, and `Stop Bits` is 1 (these are initial parameters), then open the port.

Inputs should be valid strings ending with `\n`. Commands like `conf 9600,N,8,1`, `rts 0`, `rts 1` will be executed, others will be sent back (in upper case) for verification.
