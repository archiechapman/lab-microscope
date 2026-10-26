//! USB-level description of the device and its vendor commands.
//!
//! Every command is a vendor control transfer. The firmware does not appear
//! to care about the recipient bits, so hosts should send them with the
//! *device* recipient: WebUSB validates `wIndex` against claimed interfaces
//! and endpoints for the other recipients, and several commands (notably the
//! firmware chunks, `wIndex` 0..=153) would be rejected.

pub const VENDOR_ID: u16 = 0x1871;
pub const PRODUCT_ID: u16 = 0x01B0;

pub const CONFIGURATION: u8 = 1;
pub const INTERFACE: u8 = 0;
/// Alternate setting that enables the high-bandwidth isochronous endpoint.
pub const STREAM_ALT_SETTING: u8 = 5;
/// Alternate setting with no isochronous bandwidth (idle).
pub const IDLE_ALT_SETTING: u8 = 0;
/// Endpoint *number* of the isochronous video endpoint (address 0x83, IN).
pub const ISO_ENDPOINT: u8 = 3;
/// Bytes per isochronous packet: 1024-byte packets, 3 transactions per microframe.
pub const ISO_PACKET_LEN: u32 = 3 * 1024;

/// Version string reported by the 8051 once our firmware is running.
pub const FIRMWARE_VERSION: &[u8; 12] = b"Ver R003.001";

/// Vendor `bRequest` codes.
pub mod req {
    /// IN, 8 bytes: capability block.
    pub const CAPABILITY: u8 = 0x01;
    /// OUT, 32 bytes of data, `wIndex` = chunk number.
    pub const FIRMWARE_CHUNK: u8 = 0x04;
    /// OUT, `wIndex` = 1: start the 8051 running the uploaded firmware.
    pub const BOOT: u8 = 0x05;
    /// IN, 12 bytes: firmware version string.
    pub const VERSION: u8 = 0x08;
    /// OUT, `wIndex` = 2 with `wValue` 1 starts capture; `wValue` 0 stops it.
    pub const CAPTURE: u8 = 0x22;
    /// OUT, `wValue` = width, `wIndex` = (format << 12) | height.
    pub const RESOLUTION: u8 = 0x32;
    pub const RED_LEVEL: u8 = 0x5C;
    pub const GREEN_LEVEL: u8 = 0x5D;
    pub const BLUE_LEVEL: u8 = 0x5E;
    pub const AUTO_EXPOSURE: u8 = 0x5F;
    pub const AE_TARGET: u8 = 0x69;
    pub const AE_GAIN: u8 = 0x6A;
    pub const AE_EXPOSURE: u8 = 0x6B;
    pub const COLOR_GAIN: u8 = 0x6C;
    pub const BRIGHTNESS: u8 = 0x6D;
    pub const CONTRAST: u8 = 0x6E;
    pub const SHARPNESS: u8 = 0x6F;
    pub const GAMMA_A: u8 = 0x70;
    pub const GAMMA_B: u8 = 0x71;
    pub const HUE: u8 = 0x72;
    pub const SATURATION: u8 = 0x73;
    pub const UNKNOWN_8F: u8 = 0x8F;
    pub const UNKNOWN_90: u8 = 0x90;
}

/// A vendor control-OUT transfer with no data stage, plus how long to wait
/// after it before sending the next one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub delay_ms: u32,
}

impl Request {
    pub const fn new(request: u8, value: u16, index: u16, delay_ms: u32) -> Self {
        Self {
            request,
            value,
            index,
            delay_ms,
        }
    }
}

/// Pixel format code placed in the top nibble of the resolution `wIndex`.
/// 1 = packed YUV 4:2:2 (UYVY), the only format seen so far.
const FORMAT_YUV422: u16 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resolution {
    pub width: u16,
    pub height: u16,
}

impl Resolution {
    /// Fits within the isochronous bandwidth at ~30 fps. Known good.
    pub const VGA: Self = Self {
        width: 640,
        height: 480,
    };
    /// Full sensor resolution. Exceeds the bandwidth at 30 fps, so expect a
    /// low frame rate or no frames at all. Experimental.
    pub const SXGA: Self = Self {
        width: 1280,
        height: 1024,
    };

    pub const fn frame_bytes(self) -> usize {
        self.width as usize * self.height as usize * 2
    }

    pub const fn request(self) -> Request {
        Request::new(
            req::RESOLUTION,
            self.width,
            (FORMAT_YUV422 << 12) | (self.height & 0x0FFF),
            20,
        )
    }
}

/// Number of 32-byte chunks in a firmware upload.
pub const FIRMWARE_CHUNKS: usize = 154;
pub const FIRMWARE_CHUNK_LEN: usize = 32;

/// Sent once all firmware chunks are uploaded. Wait before talking to it again.
pub const BOOT_REQUEST: Request = Request::new(req::BOOT, 0, 1, 500);

/// Commands that start streaming, to be sent after selecting
/// [`STREAM_ALT_SETTING`]: resolution, capture start, then sensor defaults.
pub fn start_sequence(resolution: Resolution) -> Vec<Request> {
    let mut seq = vec![resolution.request(), Request::new(req::CAPTURE, 1, 2, 200)];
    seq.extend_from_slice(&SENSOR_DEFAULTS);
    seq
}

/// Commands that stop streaming, sent before returning to [`IDLE_ALT_SETTING`].
pub fn stop_sequence() -> Vec<Request> {
    vec![Request::new(req::CAPTURE, 0, 0, 50)]
}

/// Sensor configuration sent by the vendor software after starting capture.
/// Values recovered by kswit from the vendor application's defaults.
pub const SENSOR_DEFAULTS: [Request; 18] = [
    Request::new(req::RED_LEVEL, 0, 0, 10),
    Request::new(req::GREEN_LEVEL, 0, 0, 10),
    Request::new(req::BLUE_LEVEL, 0, 0, 10),
    Request::new(req::HUE, 0, 1, 10),
    Request::new(req::UNKNOWN_90, 0, 0, 10),
    Request::new(req::UNKNOWN_8F, 0, 0, 10),
    Request::new(req::BLUE_LEVEL, 0, 0, 10),
    Request::new(req::COLOR_GAIN, 50, 2, 10),
    Request::new(req::AUTO_EXPOSURE, 1, 0, 200),
    Request::new(req::AE_TARGET, 50, 0, 10),
    Request::new(req::AE_GAIN, 50, 0, 10),
    Request::new(req::AE_EXPOSURE, 50, 0, 10),
    Request::new(req::CONTRAST, 3, 0, 10),
    Request::new(req::GAMMA_A, 4, 1, 10),
    Request::new(req::GAMMA_B, 5, 1, 10),
    Request::new(req::SHARPNESS, 50, 0, 10),
    Request::new(req::SATURATION, 2, 0, 10),
    Request::new(req::BRIGHTNESS, 70, 1, 10),
];

/// User-adjustable controls. Ranges are educated guesses from the defaults
/// and have not been confirmed against the vendor software.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    AutoExposure,
    AeTarget,
    AeGain,
    AeExposure,
    Brightness,
    Contrast,
    Sharpness,
    Saturation,
}

impl Control {
    pub const ALL: [Control; 8] = [
        Control::AutoExposure,
        Control::AeTarget,
        Control::AeGain,
        Control::AeExposure,
        Control::Brightness,
        Control::Contrast,
        Control::Sharpness,
        Control::Saturation,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Control::AutoExposure => "Auto exposure",
            Control::AeTarget => "AE target",
            Control::AeGain => "Gain",
            Control::AeExposure => "Exposure",
            Control::Brightness => "Brightness",
            Control::Contrast => "Contrast",
            Control::Sharpness => "Sharpness",
            Control::Saturation => "Saturation",
        }
    }

    /// Stable identifier, suitable for HTML ids.
    pub const fn key(self) -> &'static str {
        match self {
            Control::AutoExposure => "auto-exposure",
            Control::AeTarget => "ae-target",
            Control::AeGain => "ae-gain",
            Control::AeExposure => "ae-exposure",
            Control::Brightness => "brightness",
            Control::Contrast => "contrast",
            Control::Sharpness => "sharpness",
            Control::Saturation => "saturation",
        }
    }

    /// (min, max, default)
    pub const fn range(self) -> (u16, u16, u16) {
        match self {
            Control::AutoExposure => (0, 1, 1),
            Control::AeTarget | Control::AeGain | Control::AeExposure => (0, 100, 50),
            Control::Brightness => (0, 100, 70),
            Control::Contrast => (0, 7, 3),
            Control::Sharpness => (0, 100, 50),
            Control::Saturation => (0, 7, 2),
        }
    }

    pub const fn is_toggle(self) -> bool {
        matches!(self, Control::AutoExposure)
    }

    pub fn request(self, value: u16) -> Request {
        let (min, max, _) = self.range();
        let value = value.clamp(min, max);
        let (request, index) = match self {
            Control::AutoExposure => (req::AUTO_EXPOSURE, 0),
            Control::AeTarget => (req::AE_TARGET, 0),
            Control::AeGain => (req::AE_GAIN, 0),
            Control::AeExposure => (req::AE_EXPOSURE, 0),
            Control::Brightness => (req::BRIGHTNESS, 1),
            Control::Contrast => (req::CONTRAST, 0),
            Control::Sharpness => (req::SHARPNESS, 0),
            Control::Saturation => (req::SATURATION, 0),
        };
        Request::new(request, value, index, 10)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vga_resolution_matches_capture() {
        let r = Resolution::VGA.request();
        assert_eq!((r.request, r.value, r.index), (0x32, 0x0280, 0x11E0));
    }

    #[test]
    fn sxga_resolution_matches_capture() {
        let r = Resolution::SXGA.request();
        assert_eq!((r.value, r.index), (0x0500, 0x1400));
    }

    #[test]
    fn start_sequence_order() {
        let seq = start_sequence(Resolution::VGA);
        assert_eq!(seq[0].request, req::RESOLUTION);
        assert_eq!(
            (seq[1].request, seq[1].value, seq[1].index),
            (req::CAPTURE, 1, 2)
        );
        assert_eq!(seq.len(), 2 + SENSOR_DEFAULTS.len());
    }

    #[test]
    fn controls_clamp() {
        assert_eq!(Control::Contrast.request(99).value, 7);
        assert_eq!(Control::Brightness.request(70).index, 1);
    }

    #[test]
    fn defaults_match_sensor_init() {
        for c in Control::ALL {
            let (_, _, default) = c.range();
            let req = c.request(default);
            assert!(
                SENSOR_DEFAULTS.iter().any(|d| d.request == req.request
                    && d.value == req.value
                    && d.index == req.index),
                "{c:?} default not in init sequence"
            );
        }
    }
}
