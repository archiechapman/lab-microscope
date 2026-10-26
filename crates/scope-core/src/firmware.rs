//! Locating and validating the 8051 firmware image.
//!
//! The firmware is Aveo's proprietary code, so this project does not ship it.
//! Instead the user supplies a file they already have, and we search it for
//! the image. Accepted inputs:
//!
//! * a vendor driver or application file from the original CD that embeds
//!   the image as raw bytes;
//! * a raw 4928-byte firmware dump;
//! * a C source/header containing the bytes as `0xNN` literals.

use crate::protocol::{FIRMWARE_CHUNKS, FIRMWARE_CHUNK_LEN};
use sha2::{Digest, Sha256};

pub const FIRMWARE_LEN: usize = FIRMWARE_CHUNKS * FIRMWARE_CHUNK_LEN;

/// The image begins with "Aveo" followed by 0xFF padding.
pub const MAGIC: &[u8] = b"Aveo\xff\xff\xff\xff";

/// SHA-256 of the reference image published in kswit/USB_driver
/// (`Ver R003.001`, MT9M111). This is NOT yet confirmed against an original
/// vendor CD: a match means "same as kswit's copy", nothing more. Once an
/// image extracted from an original CD is confirmed to match, record that
/// here and in `CONFIRMED_AGAINST_VENDOR_MEDIA`.
pub const REFERENCE_SHA256: &[&str] =
    &["e10c93d603ceea1d2baa773ab793d63714f8ae7e94bade3ebb847bf0c4797c31"];

/// Whether the reference hash has been checked against an image extracted
/// from original vendor media.
pub const CONFIRMED_AGAINST_VENDOR_MEDIA: bool = false;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Binary,
    HexText,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Firmware {
    bytes: Vec<u8>,
    pub sha256: String,
    /// True when the hash matches [`REFERENCE_SHA256`].
    pub matches_reference: bool,
    pub source: Source,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FirmwareError {
    NotFound,
}

impl core::fmt::Display for FirmwareError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FirmwareError::NotFound => write!(
                f,
                "no AVEO firmware image found in this file; try the other driver files on the CD"
            ),
        }
    }
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl Firmware {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn chunks(&self) -> impl Iterator<Item = (u16, &[u8])> {
        self.bytes
            .chunks_exact(FIRMWARE_CHUNK_LEN)
            .enumerate()
            .map(|(i, c)| (i as u16, c))
    }

    /// Human-readable strings embedded in the image (sensor name, build date).
    pub fn description(&self) -> String {
        self.bytes
            .split(|&b| b == 0xFF || b == 0)
            .filter(|s| s.len() >= 6 && s.iter().all(|c| c.is_ascii_graphic() || *c == b' '))
            .take(4)
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// Search an arbitrary file for the firmware image.
    pub fn extract(file: &[u8]) -> Result<Self, FirmwareError> {
        if let Some(fw) = Self::scan(file, Source::Binary) {
            return Ok(fw);
        }
        let text_bytes = parse_hex_literals(file);
        if let Some(fw) = Self::scan(&text_bytes, Source::HexText) {
            return Ok(fw);
        }
        Err(FirmwareError::NotFound)
    }

    /// Prefer a candidate matching the reference hash, otherwise take the first one.
    fn scan(data: &[u8], source: Source) -> Option<Self> {
        let mut first = None;
        let mut start = 0;
        while let Some(off) = find(&data[start..], MAGIC) {
            let at = start + off;
            if let Some(img) = data.get(at..at + FIRMWARE_LEN) {
                let sha256 = sha256_hex(img);
                let matches_reference = REFERENCE_SHA256.contains(&sha256.as_str());
                let fw = Firmware {
                    bytes: img.to_vec(),
                    sha256,
                    matches_reference,
                    source,
                };
                if matches_reference {
                    return Some(fw);
                }
                first.get_or_insert(fw);
            }
            start = at + 1;
        }
        first
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Pull every `0xNN` literal out of text, in order.
fn parse_hex_literals(text: &[u8]) -> Vec<u8> {
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::new();
    let mut i = 0;
    while i + 3 < text.len() + 1 {
        if text[i] == b'0' && matches!(text.get(i + 1), Some(b'x' | b'X')) {
            if let (Some(&a), Some(&b)) = (text.get(i + 2), text.get(i + 3)) {
                let next_is_hex = text.get(i + 4).and_then(|&c| hex(c)).is_some();
                if let (Some(h), Some(l), false) = (hex(a), hex(b), next_is_hex) {
                    out.push(h << 4 | l);
                    i += 4;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_image() -> Vec<u8> {
        let mut img = vec![0x5A; FIRMWARE_LEN];
        img[..MAGIC.len()].copy_from_slice(MAGIC);
        img[128..140].copy_from_slice(b"MT9M111 0.01");
        img
    }

    #[test]
    fn finds_image_embedded_in_binary() {
        let mut file = vec![0u8; 1000];
        file.extend_from_slice(&fake_image());
        file.extend_from_slice(&[0u8; 500]);
        let fw = Firmware::extract(&file).unwrap();
        assert_eq!(fw.bytes(), &fake_image()[..]);
        assert_eq!(fw.source, Source::Binary);
        assert!(!fw.matches_reference);
        assert_eq!(fw.chunks().count(), FIRMWARE_CHUNKS);
        assert!(fw.description().contains("MT9M111"));
    }

    #[test]
    fn finds_image_in_c_header() {
        let mut text = String::from("static const u8 fw[154][32] = {\n");
        for (i, b) in fake_image().iter().enumerate() {
            text.push_str(&format!("0x{b:02x}, "));
            if i % 32 == 31 {
                text.push('\n');
            }
        }
        text.push_str("};\n");
        let fw = Firmware::extract(text.as_bytes()).unwrap();
        assert_eq!(fw.bytes(), &fake_image()[..]);
        assert_eq!(fw.source, Source::HexText);
    }

    #[test]
    fn truncated_image_rejected() {
        let img = fake_image();
        assert_eq!(Firmware::extract(&img[..100]), Err(FirmwareError::NotFound));
    }

    #[test]
    fn hex_parser_ignores_longer_literals() {
        assert_eq!(parse_hex_literals(b"0x12, 0x1234, 0xAb"), vec![0x12, 0xAB]);
    }

    /// Checks against the reference image if a copy is available locally.
    /// Set AVEO_FW=/path/to/aveo_firmware.h (or a vendor file) to run it.
    #[test]
    fn reference_image_matches() {
        let Ok(path) = std::env::var("AVEO_FW") else {
            return;
        };
        let data = std::fs::read(path).unwrap();
        let fw = Firmware::extract(&data).unwrap();
        assert!(fw.matches_reference, "hash {}", fw.sha256);
    }
}
