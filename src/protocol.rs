use std::io::{self, ErrorKind::TimedOut};
use std::time::Duration;

use clap::ValueEnum;

use async_io::{Timer, block_on};
use futures_lite::FutureExt;
use log::{debug, info};
use nusb::Interface;
use nusb::transfer::{ControlOut, ControlType, Recipient, RequestBuffer};
use zerocopy::{FromBytes, IntoBytes};
use zerocopy_derive::{FromBytes, Immutable, IntoBytes};

#[allow(non_camel_case_types)]
#[derive(ValueEnum, Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum Region {
    Sram = 0x471,
    Dram = 0x472,
}

impl std::fmt::Display for Region {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.to_possible_value()
            .expect("no values are skipped")
            .get_name()
            .fmt(f)
    }
}

const CRC: crc::Crc<u16> = crc::Crc::<u16>::new(&crc::CRC_16_IBM_3740);

const USB_REQUEST_SIGNATURE: &[u8; 4] = b"USBC";
const USB_RESPONSE_SIGNATURE: &[u8; 4] = b"USBS";

const FLAG_DIR_IN: u8 = 0x80;

#[derive(Clone, Debug, Copy, IntoBytes, Immutable)]
#[repr(u8)]
enum Command {
    UnitReady = 0x00,
    Version = 0x0c,
    Chipinfo = 0x1b,
    Capability = 0xaa,
}

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
    let _: io::Result<usize> = {
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

    let _: io::Result<usize> = {
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
        code: Command::Chipinfo as u8,
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
    let length = 0x10;

    let req = Request {
        signature: *USB_REQUEST_SIGNATURE,
        tag,
        length,
        flag: FLAG_DIR_IN,
        lun: 0,
        command_length: 6,
        command: cmd,
    };

    let r = req.as_bytes().to_vec();

    usb_send(i, e_out_addr, r);

    // The rest is just ffff...
    // NOTE: not sure if this here is always the same `length` or just
    // coincidentally in the case of the ChipInfo command.
    let d = &mut usb_read_n(i, e_in_addr, length as usize)[..4];
    d.reverse();
    let s = std::str::from_utf8(d).unwrap();
    info!("Chip ID: {s} {d:02x?}");

    let buf = &usb_read_n(i, e_in_addr, RESPONSE_SIZE);
    let (res, _) = Response::read_from_prefix(buf).unwrap();

    assert_eq!(res.signature, *USB_RESPONSE_SIGNATURE);
    let res_tag = res.tag;
    assert_eq!(res_tag, tag);

    debug!("Metadata: {res:#02x?}");
}

const CHUNK_SIZE: usize = 4096;

// TODO: Are there other requests than this?
const REQUEST: u8 = 0xc;

fn usb_out(i: &Interface, data: &[u8], region: &Region) {
    let index = region.clone() as u16; // where the mask ROM writes this;
    let out = ControlOut {
        control_type: ControlType::Vendor,
        recipient: Recipient::Device,
        request: REQUEST,
        value: 0,
        index,
        data,
    };
    let res: io::Result<usize> = {
        let timeout = Duration::from_millis(25);
        let fut = async {
            let comp = i.control_out(out).await;
            comp.status.map_err(io::Error::other)?;
            let n = comp.data.actual_length();
            Ok(n)
        };

        block_on(fut.or(async {
            Timer::after(timeout).await;
            Err(TimedOut.into())
        }))
    };

    if let Err(e) = res {
        panic!("{e:?}");
    }
}

pub fn run(i: &Interface, data: &[u8], region: &Region) {
    let mut ext_data = data.to_vec();
    // avoid splitting checksum across chunks, not sure if needed/why
    if ext_data.len() % CHUNK_SIZE == 4095 {
        ext_data.extend_from_slice(&[0]);
    }
    let checksum = CRC.checksum(&ext_data);
    // Yes, this must be big endian.
    ext_data.extend_from_slice(&checksum.to_be_bytes());
    let l = ext_data.len();
    info!("Send {l} bytes");

    let full_chunks = l / CHUNK_SIZE;
    info!("Send chunks");
    for c in 0..full_chunks {
        let o = c * CHUNK_SIZE;
        info!("Send chunk {c} at offset {o:08x}");
        let chunk = &ext_data[o..o + CHUNK_SIZE];
        debug!("  first bytes: {:02x?}", &chunk[..4]);
        debug!("  last bytes:  {:02x?}", &chunk[CHUNK_SIZE - 4..CHUNK_SIZE]);
        usb_out(i, chunk, region);
    }
    if ext_data.len() % CHUNK_SIZE > 0 {
        let o = full_chunks * CHUNK_SIZE;
        let remaining = &ext_data[o..];
        let l = remaining.len();
        info!("Send remaining data, {l} bytes");
        let f = l.min(4);
        debug!("  first bytes: {:02x?}", &remaining[..f]);
        if l > 4 {
            debug!("  last bytes:  {:02x?}", &remaining[l - 4..l]);
        }
        usb_out(i, remaining, region);
    } else {
        info!("Send extra zero-byte for 4K-aligned blob");
        usb_out(i, &[0], region);
    }
}
