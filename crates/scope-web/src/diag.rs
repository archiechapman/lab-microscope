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
/// End of the boot ROM (exclusive).
const ROM_END: u32 = 0x8000;

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

/// Request 0x01 returns one byte of memory at `wIndex`, whatever `wLength` says.
async fn read_byte(dev: &UsbDevice, addr: u16) -> Result<u8, String> {
    let b = usb::ctrl_in(dev, req::CAPABILITY, 0, addr, 1).await?;
    b.first()
        .copied()
        .ok_or_else(|| format!("empty read at {addr:04x}"))
}

async fn read_bytes(dev: &UsbDevice, addrs: &[u16]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(addrs.len());
    for &a in addrs {
        out.push(read_byte(dev, a).await?);
    }
    Ok(out)
}

/// Probe, then dump the boot ROM one byte at a time. Calls `progress` with log lines.
pub async fn dump(dev: &UsbDevice, mut progress: impl FnMut(&str)) -> Result<Vec<u8>, String> {
    let header = read_bytes(
        dev,
        &[
            FIRMWARE_BASE,
            FIRMWARE_BASE + 1,
            FIRMWARE_BASE + 2,
            FIRMWARE_BASE + 3,
        ],
    )
    .await?;
    progress(&format!("Read @8000..8003: {}", hex(&header)));
    if header != MAGIC[..4] {
        return Err(
            "request 0x01 did not return the firmware header at 0x8000; \
                    not dumping. Please send this log."
                .into(),
        );
    }

    // Only 0x0000 (which the vendor software itself reads) and 0x0003 are
    // touched before we know which address space this is.
    let v0 = read_byte(dev, 0x0000).await?;
    let v3 = read_byte(dev, 0x0003).await?;
    progress(&format!("Read @0000: {v0:02x}, @0003: {v3:02x}"));
    if !looks_like_vectors(&[v0, 0, 0, v3]) {
        return Err(
            "0x0000 does not look like 8051 code, so request 0x01 may be reading \
                    hardware registers; stopped for safety. Please send this log."
                .into(),
        );
    }

    let start = js_sys::Date::now();
    let mut out = Vec::with_capacity(ROM_END as usize);
    for addr in 0..ROM_END {
        out.push(read_byte(dev, addr as u16).await?);
        let done = addr + 1;
        if done.is_multiple_of(0x800) {
            let secs = (js_sys::Date::now() - start) / 1000.0;
            let remaining = secs / done as f64 * (ROM_END - done) as f64;
            progress(&format!(
                "Dumped {done:04x} of {ROM_END:04x} ({}%), about {:.0} s left",
                done * 100 / ROM_END,
                remaining
            ));
        }
    }
    Ok(out)
}
