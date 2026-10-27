//! Diagnostics: USB descriptor summary and a read-only dump of the bridge
//! chip's boot ROM, used to work out how to make the device stream over bulk
//! transfers (which Chromium supports on Windows) instead of isochronous.
//!
//! Vendor request 0x01 turned out to read the chip's *data* space (registers
//! at 0x0000), so it cannot reach the ROM. Instead, the firmware is uploaded
//! with a small patch (see `scope_core::patch`) that makes request 0xA0
//! return eight bytes of *code* memory, which has no read side effects.

use scope_core::firmware::MAGIC;
use scope_core::patch::{CODE_READ_LEN, CODE_READ_REQUEST};
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

async fn read_code(dev: &UsbDevice, addr: u16) -> Result<Vec<u8>, String> {
    usb::ctrl_in(dev, CODE_READ_REQUEST, 0, addr, CODE_READ_LEN).await
}

/// Dump the boot ROM through the patched firmware's code-memory reader.
/// Code memory has no read side effects, so no address is unsafe to read.
pub async fn dump(dev: &UsbDevice, mut progress: impl FnMut(&str)) -> Result<Vec<u8>, String> {
    let header = read_code(dev, FIRMWARE_BASE).await?;
    progress(&format!("Code read @8000: {}", hex(&header)));
    if header.len() != CODE_READ_LEN as usize || !header.starts_with(&MAGIC[..4]) {
        return Err(
            "the diagnostics patch is not active. Unplug the microscope, plug it \
                    back in, press Connect (the log should say 'with diagnostics patch'), \
                    then run Diagnostics again."
                .into(),
        );
    }
    let v = read_code(dev, 0x0000).await?;
    progress(&format!(
        "Code read @0000: {}{}",
        hex(&v),
        if looks_like_vectors(&v) {
            " (8051 vectors, as expected)"
        } else {
            " (unexpected, continuing)"
        }
    ));

    let start = js_sys::Date::now();
    let mut out = Vec::with_capacity(ROM_END as usize);
    let mut addr: u32 = 0;
    while addr < ROM_END {
        let b = read_code(dev, addr as u16).await?;
        if b.len() != CODE_READ_LEN as usize {
            return Err(format!("short read at {addr:04x}: {} bytes", b.len()));
        }
        out.extend_from_slice(&b);
        addr += CODE_READ_LEN as u32;
        if addr.is_multiple_of(0x800) {
            let secs = (js_sys::Date::now() - start) / 1000.0;
            let remaining = secs / addr as f64 * (ROM_END - addr) as f64;
            progress(&format!(
                "Dumped {addr:04x} of {ROM_END:04x} ({}%), about {:.0} s left",
                addr * 100 / ROM_END,
                remaining
            ));
        }
    }
    Ok(out)
}
