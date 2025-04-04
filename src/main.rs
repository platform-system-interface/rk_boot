use std::thread::sleep;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use log::{debug, info};
use nusb::{Device, Interface, Speed, transfer::Direction};

mod protocol;

const USB_VID_RK: u16 = 0x2207;
const USB_PID_RK3366: u16 = 0x350a;

const CLAIM_INTERFACE_TIMEOUT: Duration = Duration::from_secs(1);
const CLAIM_INTERFACE_PERIOD: Duration = Duration::from_micros(200);

fn claim_interface(d: &Device, ii: u8) -> std::result::Result<Interface, String> {
    let now = Instant::now();
    while Instant::now() <= now + CLAIM_INTERFACE_TIMEOUT {
        match d.claim_interface(ii) {
            Ok(i) => {
                return Ok(i);
            }
            Err(_) => {
                sleep(CLAIM_INTERFACE_PERIOD);
            }
        }
    }
    Err("failure claiming USB interface".into())
}

pub fn connect() -> (Interface, u8, u8) {
    let di = nusb::list_devices()
        .unwrap()
        .find(|d| d.vendor_id() == USB_VID_RK && d.product_id() == USB_PID_RK3366)
        .expect("Device not found, is it connected and in the right mode?");
    debug!("{di:?}");
    let ms = di.manufacturer_string().unwrap_or("[no manufacturer]");
    let ps = di.product_string().unwrap_or("[no product id]");
    info!("Found {ms} {ps}");

    // Just use the first interface
    let ii = di.interfaces().next().unwrap().interface_number();
    let d = di.open().unwrap();
    let i = claim_interface(&d, ii).unwrap();

    let speed = di.speed().unwrap();
    let packet_size = match speed {
        Speed::Full | Speed::Low => 64,
        Speed::High => 512,
        Speed::Super | Speed::SuperPlus => 1024,
        _ => panic!("Unknown USB device speed {speed:?}"),
    };
    debug!("speed {speed:?} - max packet size: {packet_size}");

    // TODO: Nice error messages when either is not found
    // We may also hardcode the endpoint to 0x01.
    let c = d.configurations().next().unwrap();
    let s = c.interface_alt_settings().next().unwrap();

    let mut es = s.endpoints();
    let e_out = es.find(|e| e.direction() == Direction::Out).unwrap();
    let e_out_addr = e_out.address();

    let mut es = s.endpoints();
    let e_in = es.find(|e| e.direction() == Direction::In).unwrap();
    let e_in_addr = e_in.address();

    for e in es {
        debug!("{e:?}");
    }

    (i, e_in_addr, e_out_addr)
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run binary code from file
    #[clap(verbatim_doc_comment)]
    Run {
        #[clap(long, short, value_enum, default_value = "sram")]
        region: protocol::Region,
        file_name: String,
    },
    /// Get chip information; requires DRAM init + usbplug binary, see
    /// https://github.com/rockchip-linux/rkbin
    Info,
}

/// Rockchip mask ROM loader tool
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Command to run
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Mode {
    UsbPlug = 1,
    MaskROM = 2,
    Unknown = 3,
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let m = match self {
            Self::UsbPlug => "USB plug",
            Self::MaskROM => "mask ROM",
            Self::Unknown => "unknown",
        };
        write!(f, "{m}")
    }
}

fn main() {
    // Default to log level "info". Otherwise, you get no "regular" logs.
    let env = env_logger::Env::default().default_filter_or("info");
    env_logger::Builder::from_env(env).init();

    let cmd = Cli::parse().cmd;

    let (i, e_in_addr, e_out_addr) = connect();

    // Good enough as a heuristic; USB plug mode also has no manufacturer string
    let mode = match e_out_addr {
        1 => Mode::UsbPlug,
        2 => Mode::MaskROM,
        _ => Mode::Unknown,
    };
    info!("Mode: {mode}");

    match cmd {
        Command::Info => {
            if mode != Mode::UsbPlug {
                panic!("Device must be in USB plug mode");
            }
            protocol::info(&i, e_in_addr, e_out_addr);
        }
        Command::Run { file_name, region } => {
            let data = std::fs::read(file_name).unwrap();
            protocol::run(&i, &data, &region);
        }
    }
}
