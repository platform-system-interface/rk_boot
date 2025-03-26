use std::io::{self, ErrorKind::TimedOut, Read, Result};
use std::thread;
use std::time::{Duration, Instant};

use async_io::{Timer, block_on};
use clap::{Parser, Subcommand, ValueEnum};
use futures_lite::FutureExt;
use log::{debug, error, info};
use nusb::{
    Device, Interface, Speed,
    transfer::{Direction, RequestBuffer},
};

const USB_VID_RK: u16 = 0x2207;
const USB_PID_RK3366: u16 = 0x350a;

const CLAIM_INTERFACE_TIMEOUT: Duration = Duration::from_secs(1);
const CLAIM_INTERFACE_PERIOD: Duration = Duration::from_micros(200);

pub fn connect() -> (Interface, u8, u8) {
    let di = nusb::list_devices()
        .unwrap()
        .find(|d| d.vendor_id() == USB_VID_RK && d.product_id() == USB_PID_RK3366)
        .expect("Device not found, is it connected and in the right mode?");
    info!("{di:?}");
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

const USB_REQUEST_SIGN: usize = 0x55534243;

const OPCODE_READ_CAPABILITY: u8 = 0xaa;

const REQ_TYPE_IN: u8 = 0xc0;

fn usb_send(i: &Interface, addr: u8, data: Vec<u8>) {
    let _: Result<usize> = {
        let timeout = Duration::from_secs(5);
        let fut = async {
            let comp = i.bulk_out(addr, data).await;
            comp.status.map_err(io::Error::other)?;
            let n = comp.data.actual_length();
            Ok(n)
        };

        block_on(fut.or(async {
            Timer::after(timeout).await;
            Err(TimedOut.into())
        }))
    };
}

pub fn info(i: &Interface, e_out_addr: u8) {
    println!("Read chip info");
    // This appears to be constant?!
    let mut buf = vec![0; 8];
    usb_send(i, e_out_addr, buf);
    i.control_out();
    /*
    match h.read_control(
        rusb::constants::LIBUSB_ENDPOINT_IN,
        OPCODE_READ_CAPABILITY,
        0,
        0,
        &mut buf,
        t,
    ) {
        Ok(_) => {
            for i in (0..buf.len()).step_by(4) {
                let r = u32::from_le_bytes(buf[i..i + 4].try_into().unwrap());
                println!("  {r:04x?}");
            }
        }
        Err(e) => println!("info cmd err: {e:?}"),
    }
    */
}

fn claim_interface(d: &Device, ii: u8) -> std::result::Result<Interface, String> {
    let now = Instant::now();
    while Instant::now() <= now + CLAIM_INTERFACE_TIMEOUT {
        match d.claim_interface(ii) {
            Ok(i) => {
                return Ok(i);
            }
            Err(_) => {
                thread::sleep(CLAIM_INTERFACE_PERIOD);
            }
        }
    }
    Err("failure claiming USB interface".into())
}

fn main() {
    // Default to log level "info". Otherwise, you get no "regular" logs.
    let env = env_logger::Env::default().default_filter_or("info");
    env_logger::Builder::from_env(env).init();

    let cmd = Cli::parse().cmd;

    let (i, e_in_addr, e_out_addr) = connect();

    match cmd {
        Command::Info => {
            println!("\n=======\n");
            info(&i, e_out_addr);
            println!();
        }
    }

    println!("{cmd:?}");
}
