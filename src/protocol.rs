use std::io::{self, ErrorKind::TimedOut, Read, Result};
use std::time::Duration;

use async_io::{Timer, block_on};
use futures_lite::FutureExt;
use log::{debug, error, info};
use nusb::{Interface, transfer::RequestBuffer};
use zerocopy::{FromBytes, IntoBytes};
use zerocopy_derive::{FromBytes, Immutable, IntoBytes};

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
