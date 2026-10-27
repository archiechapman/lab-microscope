//! Diagnostics: USB descriptor summary and a read-only dump of the bridge
//! chip's boot ROM, used to work out how to make the device stream over bulk
//! transfers (which Chromium supports on Windows) instead of isochronous.
//!
//! The dump uses vendor request 0x01, which kswit's tools use with an address
//! in `wIndex`. Before dumping, we check two things so that we never read
//! hardware registers with side effects:
//! 1. reading 0x8000 returns the uploaded firmware header ("Aveo"), proving
//!    the request reads memory at `wIndex`;
//! 2. reading 0x0000 looks like an 8051 vector table, proving it reads the
//!    *code* space (side-effect free), not the register-mapped data space.

use scope_core::firmware::MAGIC;
use scope_core::protocol::req;
use web_sys::UsbDevice;

use crate::usb;

/// Start of the uploaded firmware in the chip's memory map.
const FIRMWARE_BASE: u16 = 0x8000;
/// Dump the ROM, the firmware RAM and the variable area above it.
const DUMP_END: u32 = 0xA000;

pub fn describe(dev: &UsbDevice) -> Vec<String> {
    let mut lines = vec![format!(
        "Device {:04x}:{:04x} '{}' by '{}', USB {}.{}{}",
        dev.vendor_id(),
        dev.product_id(),
        dev.product_name().unwrap_or_default(),
        dev.manufacturer_name().unwrap_or_default(),
        dev.usb_version_major(),
        dev.usb_version_minor(),
        dev.usb_version_subminor(),
    )];
    let Some(cfg) = dev.configuration() else {
        lines.push("No active configuration".into());
        return lines;
    };
    for iface in cfg.interfaces().iter() {
        for alt in iface.alternates().iter() {
            let eps: Vec<String> = alt
                .endpoints()
                .iter()
                .map(|e| {
                    format!(
                        "EP{} {:?} {:?} {}B",
                        e.endpoint_number(),
                        e.direction(),
                        e.type_(),
                        e.packet_size()
                    )
                })
                .collect();
            lines.push(format!(
                "Interface {} alt {}: class {:02x}/{:02x}/{:02x} [{}]",
                iface.interface_number(),
                alt.alternate_setting(),
                alt.interface_class(),
                alt.interface_subclass(),
                alt.interface_protocol(),
                if eps.is_empty() {
                    "no endpoints".into()
                } else {
                    eps.join(", ")
                }
            ));
        }
    }
    lines
}

fn hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 8051 reset/interrupt vectors are LJMP (0x02) or AJMP (xxx00001) at 0x00,
/// 0x03, 0x0B... A blank vector may be RETI (0x32) or 0xFF.
fn looks_like_vectors(b: &[u8]) -> bool {
    let is_jump = |x: u8| x == 0x02 || x & 0x1F == 0x01;
    b.len() >= 4 && is_jump(b[0]) && (is_jump(b[3]) || b[3] == 0x32 || b[3] == 0xFF)
}

async fn read(dev: &UsbDevice, addr: u16, len: u16) -> Result<Vec<u8>, String> {
    usb::ctrl_in(dev, req::CAPABILITY, 0, addr, len).await
}

/// Probe, then dump code memory. Calls `progress` with log lines.
pub async fn dump(dev: &UsbDevice, mut progress: impl FnMut(&str)) -> Result<Vec<u8>, String> {
    let fw = read(dev, FIRMWARE_BASE, 8).await?;
    progress(&format!("Read @8000: {}", hex(&fw)));
    if !fw.starts_with(&MAGIC[..4]) {
        return Err(
            "request 0x01 did not return the firmware header at 0x8000; \
                    not dumping. Please send this log."
                .into(),
        );
    }
    let vec0 = read(dev, 0x0000, 8).await?;
    progress(&format!("Read @0000: {}", hex(&vec0)));
    if !looks_like_vectors(&vec0) {
        return Err(
            "0x0000 does not look like 8051 code, so request 0x01 may be reading \
                    hardware registers; stopped for safety. Please send this log."
                .into(),
        );
    }

    // Find the largest read size the ROM honours.
    let mut step: u16 = 8;
    for size in [64u16, 32, 16] {
        if let Ok(b) = read(dev, 0x0000, size).await {
            if b.len() == size as usize && b[..8] == vec0[..] {
                step = size;
                break;
            }
        }
    }
    progress(&format!("Reading {step} bytes per request"));

    let mut out = Vec::with_capacity(DUMP_END as usize);
    let mut addr: u32 = 0;
    while addr < DUMP_END {
        let b = read(dev, addr as u16, step).await?;
        if b.len() != step as usize {
            return Err(format!("short read at {addr:04x}: {} bytes", b.len()));
        }
        out.extend_from_slice(&b);
        addr += step as u32;
        if addr.is_multiple_of(0x1000) {
            progress(&format!("Dumped {:04x} of {:04x}", addr, DUMP_END));
        }
    }
    Ok(out)
}
