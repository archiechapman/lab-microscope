//! Runtime patches to the vendor firmware: a code-memory reader
//! ([`with_code_reader`]) and an experimental bulk video endpoint
//! ([`with_bulk_video`]).
//!
//! Vendor request 0x01 reads the bridge chip's *data* space, so it cannot
//! reach the boot ROM, which lives in *code* space. This patch redirects the
//! rarely used vendor request 0xA0 to a 30-byte routine of ours that returns
//! eight bytes of code memory starting at `wIndex`.
//!
//! The patch is applied in memory to the user's own firmware image just
//! before upload; nothing of the vendor's is distributed. It is applied only
//! when every patch site holds exactly the expected original bytes.
//!
//! Routine, placed in an unused 0xFF gap at 0x8060:
//!
//! ```text
//! 8060  90 01 bc   mov  dptr, #0x01bc   ; wIndex low byte (setup packet copy)
//! 8063  e0         movx a, @dptr
//! 8064  fe         mov  r6, a
//! 8065  a3         inc  dptr
//! 8066  e0         movx a, @dptr        ; wIndex high byte
//! 8067  ff         mov  r7, a
//! 8068  7d 08      mov  r5, #8
//! 806a  8e 82      mov  dpl, r6         ; loop:
//! 806c  8f 83      mov  dph, r7
//! 806e  e4         clr  a
//! 806f  93         movc a, @a+dptr      ; read code memory
//! 8070  90 00 07   mov  dptr, #0x0007   ; EP0 FIFO
//! 8073  f0         movx @dptr, a
//! 8074  0e         inc  r6
//! 8075  be 00 01   cjne r6, #0, +1
//! 8078  0f         inc  r7
//! 8079  dd ef      djnz r5, loop
//! 807b  02 85 ac   ljmp 0x85ac          ; firmware's common "EP0 done" exit
//! ```

use crate::firmware::FIRMWARE_LEN;

/// Vendor request (IN, 8 bytes) served by the patched firmware.
pub const CODE_READ_REQUEST: u8 = 0xA0;
pub const CODE_READ_LEN: u16 = 8;

const BASE: usize = 0x8000;
const ROUTINE_AT: usize = 0x8060;
const ROUTINE: [u8; 30] = [
    0x90, 0x01, 0xBC, 0xE0, 0xFE, 0xA3, 0xE0, 0xFF, 0x7D, 0x08, 0x8E, 0x82, 0x8F, 0x83, 0xE4, 0x93,
    0x90, 0x00, 0x07, 0xF0, 0x0E, 0xBE, 0x00, 0x01, 0x0F, 0xDD, 0xEF, 0x02, 0x85, 0xAC,
];
/// Switch-table entry for request 0xA0: target address (big-endian) and case value.
const TABLE_ENTRY_AT: usize = 0x83F1;
const TABLE_ORIGINAL: [u8; 3] = [0x85, 0x6A, 0xA0];
const TABLE_PATCHED: [u8; 3] = [0x80, 0x60, 0xA0];
/// The exit the routine jumps to must look like the firmware's EP0-done code:
/// `mov dptr,#0x0005; movx a,@dptr; orl a,#0x04; movx @dptr,a; ret`.
const EXIT_AT: usize = 0x85AC;
const EXIT_EXPECTED: [u8; 8] = [0x90, 0x00, 0x05, 0xE0, 0x44, 0x04, 0xF0, 0x22];

fn at(addr: usize) -> usize {
    addr - BASE
}

/// Returns a patched copy, or `None` if the image does not have the exact
/// layout this patch was written for.
pub fn with_code_reader(image: &[u8]) -> Option<Vec<u8>> {
    if image.len() != FIRMWARE_LEN {
        return None;
    }
    let gap = image.get(at(ROUTINE_AT)..at(ROUTINE_AT) + ROUTINE.len())?;
    let table = image.get(at(TABLE_ENTRY_AT)..at(TABLE_ENTRY_AT) + 3)?;
    let exit = image.get(at(EXIT_AT)..at(EXIT_AT) + EXIT_EXPECTED.len())?;
    if gap.iter().any(|&b| b != 0xFF) || table != TABLE_ORIGINAL || exit != EXIT_EXPECTED {
        return None;
    }
    let mut out = image.to_vec();
    out[at(ROUTINE_AT)..at(ROUTINE_AT) + ROUTINE.len()].copy_from_slice(&ROUTINE);
    out[at(TABLE_ENTRY_AT)..at(TABLE_ENTRY_AT) + 3].copy_from_slice(&TABLE_PATCHED);
    Some(out)
}

/// Experimental patch that makes the video endpoint 0x83 bulk instead of
/// isochronous, so that hosts without isochronous support (Chromium on
/// Windows) can read it.
///
/// The boot ROM serves fixed descriptors that declare only isochronous
/// endpoints, so the firmware's INT0 handler is redirected (at 0x89DB, where
/// it would call ROM entry 0x0070) to a hook of ours. At high speed, the hook
/// answers `GET_DESCRIPTOR(CONFIGURATION)` itself with [`BULK_CONFIG`] and
/// hands every other request to the ROM. The firmware's stream start for
/// alt setting 0 then programs EP3 as bulk 512 (`0x0031` = 0x42) instead of
/// a zero-size isochronous endpoint.
///
/// The hook does not fit in the image's gaps, so it is appended after the
/// image at 0x9340. The ROM's upload request writes chunk `n` to
/// 0x8000 + 32·n without a bounds check, and nothing uses that RAM.
/// After upload and boot, the host sends [`RECONNECT_REQUEST`]; the device
/// detaches, reattaches and is enumerated again with [`BULK_CONFIG`].
/// Each frame then arrives on 0x83 as the marker, a 6-byte header and the
/// pixels.
///
/// ```text
/// 9340  90 01 b8   mov  dptr, #0x01b8   ; bmRequestType
/// 9343  e0         movx a, @dptr
/// 9344  b4 80 40   cjne a, #0x80, rom
/// 9347  a3         inc  dptr            ; bRequest
/// 9348  e0         movx a, @dptr
/// 9349  b4 06 3b   cjne a, #0x06, rom
/// 934c  90 01 bb   mov  dptr, #0x01bb   ; descriptor type (wValue high)
/// 934f  e0         movx a, @dptr
/// 9350  b4 02 34   cjne a, #0x02, rom
/// 9353  90 00 00   mov  dptr, #0x0000   ; bit 7: high speed
/// 9356  e0         movx a, @dptr
/// 9357  30 e7 2d   jnb  acc.7, rom
/// 935a  7d 19      mov  r5, #25         ; length = min(wLength, 25)
/// 935c  90 01 bf   mov  dptr, #0x01bf
/// 935f  e0         movx a, @dptr
/// 9360  70 0a      jnz  send
/// 9362  90 01 be   mov  dptr, #0x01be
/// 9365  e0         movx a, @dptr
/// 9366  b4 19 00   cjne a, #25, +0      ; carry = wLength < 25
/// 9369  50 01      jnc  send
/// 936b  fd         mov  r5, a
/// 936c  90 00 05   mov  dptr, #0x0005   ; send: as the ROM does for
/// 936f  e0         movx a, @dptr        ;  standard requests
/// 9370  44 01      orl  a, #0x01
/// 9372  f0         movx @dptr, a
/// 9373  7e 00      mov  r6, #0
/// 9375  ed         mov  a, r5
/// 9376  60 0c      jz   done
/// 9378  90 93 8a   mov  dptr, #desc     ; loop:
/// 937b  ee         mov  a, r6
/// 937c  93         movc a, @a+dptr
/// 937d  90 00 07   mov  dptr, #0x0007   ; EP0 FIFO
/// 9380  f0         movx @dptr, a
/// 9381  0e         inc  r6
/// 9382  dd f4      djnz r5, loop
/// 9384  02 85 ac   ljmp 0x85ac          ; done: firmware's "EP0 done" exit
/// 9387  02 93 a3   ljmp reconnect       ; rom: see RECONNECT
/// 938a             desc: BULK_CONFIG
/// ```
pub fn with_bulk_video(image: &[u8]) -> Option<Vec<u8>> {
    if image.len() != FIRMWARE_LEN
        || image.get(at(ISR_CALL_AT)..at(ISR_CALL_AT) + 3)? != ISR_CALL_ORIGINAL
        || image.get(at(ALT0_EP3_AT)..at(ALT0_EP3_AT) + 4)? != ALT0_EP3_ORIGINAL
        || image.get(at(EXIT_AT)..at(EXIT_AT) + EXIT_EXPECTED.len())? != EXIT_EXPECTED
    {
        return None;
    }
    let mut out = image.to_vec();
    out[at(ISR_CALL_AT)..at(ISR_CALL_AT) + 3].copy_from_slice(&[
        0x12,
        (HOOK_AT >> 8) as u8,
        HOOK_AT as u8,
    ]);
    out[at(ALT0_EP3_AT) + 2] = EP3_BULK_IN_512;
    debug_assert_eq!(out.len(), at(HOOK_AT));
    out.extend_from_slice(&HOOK);
    out.extend_from_slice(&BULK_CONFIG);
    out.extend_from_slice(&RECONNECT);
    let padded = out.len().div_ceil(crate::protocol::FIRMWARE_CHUNK_LEN)
        * crate::protocol::FIRMWARE_CHUNK_LEN;
    out.resize(padded, 0x00);
    Some(out)
}

/// `lcall 0x0070` in the firmware's INT0 handler, after it has copied the
/// SETUP packet to 0x01B8.
const ISR_CALL_AT: usize = 0x89DB;
const ISR_CALL_ORIGINAL: [u8; 3] = [0x12, 0x00, 0x70];
/// `inc dptr; mov a,#0x20; movx @dptr,a`: stream start writes EP3's type
/// and size byte (0x0031) for alt setting 0.
const ALT0_EP3_AT: usize = 0x8629;
const ALT0_EP3_ORIGINAL: [u8; 4] = [0xA3, 0x74, 0x20, 0xF0];
/// Bulk, 512 bytes, bit 7 clear like the ROM's isochronous values for EP3.
/// EP2's `0xC2` (bit 7 set) delivers no video on EP3; tested on hardware.
const EP3_BULK_IN_512: u8 = 0x42;

const HOOK_AT: usize = BASE + FIRMWARE_LEN;
const HOOK_LEN: usize = 74;
const DESC_AT: usize = HOOK_AT + HOOK_LEN;
const HOOK: [u8; HOOK_LEN] = [
    0x90,
    0x01,
    0xB8,
    0xE0,
    0xB4,
    0x80,
    0x40, // bmRequestType
    0xA3,
    0xE0,
    0xB4,
    0x06,
    0x3B, // bRequest
    0x90,
    0x01,
    0xBB,
    0xE0,
    0xB4,
    0x02,
    0x34, // descriptor type
    0x90,
    0x00,
    0x00,
    0xE0,
    0x30,
    0xE7,
    0x2D, // high speed
    0x7D,
    BULK_CONFIG.len() as u8, // length
    0x90,
    0x01,
    0xBF,
    0xE0,
    0x70,
    0x0A, //
    0x90,
    0x01,
    0xBE,
    0xE0,
    0xB4,
    BULK_CONFIG.len() as u8,
    0x00,
    0x50,
    0x01,
    0xFD, //
    0x90,
    0x00,
    0x05,
    0xE0,
    0x44,
    0x01,
    0xF0, // send
    0x7E,
    0x00,
    0xED,
    0x60,
    0x0C, //
    0x90,
    (DESC_AT >> 8) as u8,
    DESC_AT as u8,
    0xEE,
    0x93, // loop
    0x90,
    0x00,
    0x07,
    0xF0,
    0x0E,
    0xDD,
    0xF4, //
    0x02,
    (EXIT_AT >> 8) as u8,
    EXIT_AT as u8, // done
    0x02,
    (RECONNECT_AT >> 8) as u8,
    RECONNECT_AT as u8, // rom (via the reconnect check)
];

/// Vendor request (OUT, no data) that makes the patched firmware drop off
/// the bus for about a second and reattach, so that the host enumerates it
/// again and reads [`BULK_CONFIG`]. The firmware stays in RAM.
pub const RECONNECT_REQUEST: u8 = 0xA1;

/// Every request the bulk hook does not answer passes through here.
///
/// ```text
/// 93a3  90 01 b8   mov  dptr, #0x01b8
/// 93a6  e0         movx a, @dptr
/// 93a7  b4 40 25   cjne a, #0x40, rom   ; vendor, device, OUT
/// 93aa  a3         inc  dptr
/// 93ab  e0         movx a, @dptr
/// 93ac  b4 a1 20   cjne a, #0xa1, rom
/// 93af  90 00 05   mov  dptr, #0x0005   ; complete the request first
/// 93b2  e0         movx a, @dptr
/// 93b3  44 04      orl  a, #0x04
/// 93b5  f0         movx @dptr, a
/// 93b6  7d 01      mov  r5, #1          ; let the status stage go out
/// 93b8  12 93 d2   lcall delay
/// 93bb  90 00 00   mov  dptr, #0x0000
/// 93be  e0         movx a, @dptr
/// 93bf  44 08      orl  a, #0x08        ; bit 3: detach from the bus
/// 93c1  f0         movx @dptr, a
/// 93c2  7d 40      mov  r5, #0x40       ; about 1 s
/// 93c4  12 93 d2   lcall delay
/// 93c7  90 00 00   mov  dptr, #0x0000
/// 93ca  e0         movx a, @dptr
/// 93cb  54 f7      anl  a, #0xf7        ; reattach
/// 93cd  f0         movx @dptr, a
/// 93ce  22         ret                  ; back to the INT0 handler
/// 93cf  02 00 70   ljmp 0x0070          ; rom: the original call target
/// 93d2  7e 00      mov  r6, #0          ; delay: r5 × 65536 iterations
/// 93d4  7f 00      mov  r7, #0
/// 93d6  df fe      djnz r7, $
/// 93d8  de fc      djnz r6, 93d6
/// 93da  dd f6      djnz r5, 93d2
/// 93dc  22         ret
/// ```
const RECONNECT_AT: usize = DESC_AT + BULK_CONFIG.len();
const DELAY_AT: usize = RECONNECT_AT + 47;
const RECONNECT: [u8; 58] = [
    0x90,
    0x01,
    0xB8,
    0xE0,
    0xB4,
    0x40,
    0x25,
    0xA3,
    0xE0,
    0xB4,
    RECONNECT_REQUEST,
    0x20, //
    0x90,
    0x00,
    0x05,
    0xE0,
    0x44,
    0x04,
    0xF0, //
    0x7D,
    0x01,
    0x12,
    (DELAY_AT >> 8) as u8,
    DELAY_AT as u8, //
    0x90,
    0x00,
    0x00,
    0xE0,
    0x44,
    0x08,
    0xF0, //
    0x7D,
    0x40,
    0x12,
    (DELAY_AT >> 8) as u8,
    DELAY_AT as u8, //
    0x90,
    0x00,
    0x00,
    0xE0,
    0x54,
    0xF7,
    0xF0,
    0x22, //
    0x02,
    0x00,
    0x70, // rom
    0x7E,
    0x00,
    0x7F,
    0x00,
    0xDF,
    0xFE,
    0xDE,
    0xFC,
    0xDD,
    0xF6,
    0x22, // delay
];

/// Configuration 1 with one interface (vendor class, no alternate settings)
/// and one bulk IN endpoint 0x83 of 512 bytes, high speed only.
pub const BULK_CONFIG: [u8; 25] = [
    0x09, 0x02, 0x19, 0x00, 0x01, 0x01, 0x00, 0x80, 0xFA, // configuration, 500 mA
    0x09, 0x04, 0x00, 0x00, 0x01, 0xFF, 0xFF, 0xFF, 0x00, // interface 0, alt 0
    0x07, 0x05, 0x83, 0x02, 0x00, 0x02, 0x00, // EP 0x83, bulk, 512
];

#[cfg(test)]
mod tests {
    use super::*;

    fn layout_like_vendor() -> Vec<u8> {
        let mut img = vec![0x00; FIRMWARE_LEN];
        img[at(ROUTINE_AT)..at(ROUTINE_AT) + 32].fill(0xFF);
        img[at(TABLE_ENTRY_AT)..at(TABLE_ENTRY_AT) + 3].copy_from_slice(&TABLE_ORIGINAL);
        img[at(EXIT_AT)..at(EXIT_AT) + 8].copy_from_slice(&EXIT_EXPECTED);
        img
    }

    #[test]
    fn patches_matching_layout() {
        let img = layout_like_vendor();
        let p = with_code_reader(&img).unwrap();
        assert_eq!(&p[at(ROUTINE_AT)..at(ROUTINE_AT) + 30], &ROUTINE);
        assert_eq!(
            &p[at(TABLE_ENTRY_AT)..at(TABLE_ENTRY_AT) + 3],
            &TABLE_PATCHED
        );
        // Nothing else changes.
        let diff = img.iter().zip(&p).filter(|(a, b)| a != b).count();
        assert!(diff <= 30 + 2);
    }

    #[test]
    fn refuses_other_layouts() {
        let mut img = layout_like_vendor();
        img[at(ROUTINE_AT) + 5] = 0x12;
        assert!(with_code_reader(&img).is_none());
        let mut img = layout_like_vendor();
        img[at(TABLE_ENTRY_AT)] = 0x84;
        assert!(with_code_reader(&img).is_none());
        assert!(with_code_reader(&[0xFF; 100]).is_none());
    }

    #[test]
    fn routine_jump_offsets() {
        // cjne at 0x8075 skips one byte to 0x8079.
        assert_eq!(ROUTINE[0x8075 - ROUTINE_AT + 2], 0x01);
        // djnz at 0x8079 returns to 0x806a: rel = 0x806a - 0x807b.
        let rel = ROUTINE[0x8079 - ROUTINE_AT + 1] as i8 as isize;
        assert_eq!(0x807b + rel, 0x806a);
        // Table entry points at the routine.
        assert_eq!(
            ((TABLE_PATCHED[0] as usize) << 8) | TABLE_PATCHED[1] as usize,
            ROUTINE_AT
        );
        assert_eq!(&ROUTINE[27..], &[0x02, (EXIT_AT >> 8) as u8, EXIT_AT as u8]);
    }

    /// With AVEO_FW pointing at a real image, check the patch applies to it.
    #[test]
    fn applies_to_reference_image() {
        let Ok(path) = std::env::var("AVEO_FW") else {
            return;
        };
        let data = std::fs::read(path).unwrap();
        let fw = crate::Firmware::extract(&data).unwrap();
        assert!(with_code_reader(fw.bytes()).is_some());
    }

    fn vendor_like_for_bulk() -> Vec<u8> {
        let mut img = layout_like_vendor();
        img[at(ISR_CALL_AT)..at(ISR_CALL_AT) + 3].copy_from_slice(&ISR_CALL_ORIGINAL);
        img[at(ALT0_EP3_AT)..at(ALT0_EP3_AT) + 4].copy_from_slice(&ALT0_EP3_ORIGINAL);
        img
    }

    #[test]
    fn bulk_patch_layout() {
        let img = vendor_like_for_bulk();
        let p = with_bulk_video(&img).unwrap();
        assert_eq!(p.len() % 32, 0);
        assert_eq!(
            &p[at(ISR_CALL_AT)..at(ISR_CALL_AT) + 3],
            &[0x12, 0x93, 0x40]
        );
        assert_eq!(p[at(ALT0_EP3_AT) + 2], 0x42);
        assert_eq!(&p[at(HOOK_AT)..at(HOOK_AT) + HOOK.len()], &HOOK);
        assert_eq!(
            &p[at(DESC_AT)..at(DESC_AT) + BULK_CONFIG.len()],
            &BULK_CONFIG
        );
        assert_eq!(
            &p[at(RECONNECT_AT)..at(RECONNECT_AT) + RECONNECT.len()],
            &RECONNECT
        );
        // The original image is otherwise untouched.
        let diff = img.iter().zip(&p).filter(|(a, b)| a != b).count();
        assert_eq!(diff, 3);
        assert!(with_bulk_video(&p).is_none());
        assert!(with_bulk_video(&layout_like_vendor()).is_none());
    }

    #[test]
    fn bulk_hook_branches() {
        // (offset of instruction, length, offset of target) for each relative branch.
        let rom = HOOK.len() - 3;
        let send = 0x2C;
        let done = 0x44;
        let lp = 0x38;
        let branches = [
            (0x04, 3, rom),
            (0x09, 3, rom),
            (0x10, 3, rom),
            (0x17, 3, rom),
            (0x20, 2, send),
            (0x26, 3, 0x29),
            (0x29, 2, send),
            (0x36, 2, done),
            (0x42, 2, lp),
        ];
        for (pos, len, target) in branches {
            let rel = HOOK[pos + len - 1] as i8 as isize;
            assert_eq!(
                (pos + len) as isize + rel,
                target as isize,
                "branch at hook+{pos:#x}"
            );
        }
        assert_eq!(&HOOK[done..done + 3], &[0x02, 0x85, 0xAC]);
        assert_eq!(&HOOK[rom..], &[0x02, 0x93, 0xA3]);
        assert_eq!(HOOK[lp], 0x90);
        assert_eq!(DESC_AT, 0x938A);
        // wTotalLength matches the descriptor.
        assert_eq!(BULK_CONFIG[2] as usize, BULK_CONFIG.len());
    }

    #[test]
    fn reconnect_branches() {
        let rom = 44;
        let delay = 47;
        // (offset of instruction, length, offset of target) for each relative branch.
        let branches = [
            (4, 3, rom),
            (9, 3, rom),
            (delay + 4, 2, delay + 4),
            (delay + 6, 2, delay + 4),
            (delay + 8, 2, delay),
        ];
        for (pos, len, target) in branches {
            let rel = RECONNECT[pos + len - 1] as i8 as isize;
            assert_eq!(
                (pos + len) as isize + rel,
                target as isize,
                "branch at reconnect+{pos}"
            );
        }
        assert_eq!(RECONNECT_AT, 0x93A3);
        assert_eq!(DELAY_AT, 0x93D2);
        assert_eq!(&RECONNECT[rom..rom + 3], &[0x02, 0x00, 0x70]);
        // Both delay calls target the delay routine.
        for call in [21, 33] {
            assert_eq!(&RECONNECT[call..call + 3], &[0x12, 0x93, 0xD2]);
        }
        assert_eq!(RECONNECT[10], RECONNECT_REQUEST);
        assert_eq!(RECONNECT[43], 0x22);
        assert_eq!(*RECONNECT.last().unwrap(), 0x22);
    }

    #[test]
    fn bulk_applies_to_reference_image() {
        let Ok(path) = std::env::var("AVEO_FW") else {
            return;
        };
        let data = std::fs::read(path).unwrap();
        let fw = crate::Firmware::extract(&data).unwrap();
        assert!(with_bulk_video(fw.bytes()).is_some());
    }
}
