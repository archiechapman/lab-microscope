//! Runtime patch that adds a code-memory reader to the vendor firmware.
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
}
