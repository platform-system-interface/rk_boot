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
use zerocopy::{FromBytes, IntoBytes};
use zerocopy_derive::{FromBytes, Immutable, IntoBytes};

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
                thread::sleep(CLAIM_INTERFACE_PERIOD);
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

const USB_REQUEST_SIGNATURE: &[u8; 4] = b"USBC";
const USB_RESPONSE_SIGNATURE: &[u8; 4] = b"USBS";

const OPCODE_READ_CAPABILITY: u8 = 0xaa;

const REQ_TYPE_IN: u8 = 0xc0;

const CMD_CHIP_INFO: u8 = 0x1b;

const FLAG_DIR_IN: u8 = 0x80;

#[derive(Clone, Debug, Copy, FromBytes, IntoBytes, Immutable)]
#[repr(C, packed)]
struct RkCommand {
    code: u8,
    subcode: u8,
    address: u32,
    _r6: u8,
    size: u16,
    _r9: u8,
    _r10: u8,
    _r11: u8,
    _r12: u32,
}

#[derive(Clone, Debug, Copy, FromBytes, IntoBytes, Immutable)]
#[repr(C, packed)]
struct Request {
    signature: [u8; 4],
    tag: u32,
    length: u32,
    flag: u8,
    lun: u8,
    command_length: u8,
    command: RkCommand,
}

#[derive(Clone, Debug, Copy, FromBytes, IntoBytes, Immutable)]
#[repr(C, packed)]
struct Response {
    signature: [u8; 4],
    tag: u32,
    residue: u32,
    status: u8,
}

const RESPONSE_SIZE: usize = std::mem::size_of::<Response>();

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

fn usb_read_n(i: &Interface, addr: u8, size: usize) -> Vec<u8> {
    let mut buf = vec![0_u8; size];

    let _: Result<usize> = {
        let timeout = Duration::from_secs(5);
        let fut = async {
            let b = RequestBuffer::new(size);
            let comp = i.bulk_in(addr, b).await;
            comp.status.map_err(io::Error::other)?;

            let n = comp.data.len();
            buf[..n].copy_from_slice(&comp.data);
            Ok(n)
        };

        block_on(fut.or(async {
            Timer::after(timeout).await;
            Err(TimedOut.into())
        }))
    };

    let l = if buf.len() < 128 { buf.len() } else { 128 };
    let b = &buf[..l];
    debug!("Device says: {b:02x?}");

    buf
}

pub fn info(i: &Interface, e_in_addr: u8, e_out_addr: u8) {
    info!("Read chip info");

    let cmd = RkCommand {
        code: CMD_CHIP_INFO,
        subcode: 0,
        address: 0,
        _r6: 0,
        size: 0,
        _r9: 0,
        _r10: 0,
        _r11: 0,
        _r12: 0,
    };

    let tag = 0x13372342;

    let req = Request {
        signature: *USB_REQUEST_SIGNATURE,
        tag,
        length: 0x10,
        flag: FLAG_DIR_IN,
        lun: 0,
        command_length: 6,
        command: cmd,
    };

    let r = req.as_bytes().to_vec();

    usb_send(i, e_out_addr, r);

    let d = &mut usb_read_n(i, e_in_addr, 0x10)[..4];
    d.reverse();
    let s = std::str::from_utf8(&d).unwrap();
    info!("{s} {d:02x?}");

    let buf = &usb_read_n(i, e_in_addr, RESPONSE_SIZE);
    let (res, _) = Response::read_from_prefix(buf).unwrap();

    assert_eq!(res.signature, *USB_RESPONSE_SIGNATURE);
    let res_tag = res.tag;
    assert_eq!(res_tag, tag);

    info!("{res:#02x?}");
}

fn main() {
    // Default to log level "info". Otherwise, you get no "regular" logs.
    let env = env_logger::Env::default().default_filter_or("info");
    env_logger::Builder::from_env(env).init();

    let cmd = Cli::parse().cmd;

    let (i, e_in_addr, e_out_addr) = connect();

    match cmd {
        Command::Info => info(&i, e_in_addr, e_out_addr),
    }
}
