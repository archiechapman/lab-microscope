//! Reassembles video frames from isochronous packet payloads.
//!
//! Stream layout: each frame starts with the marker `00 FF FF FF`, then a
//! 10-byte header (contents not yet decoded), then `width * height * 2`
//! bytes of packed YUV 4:2:2.

pub const SYNC: [u8; 4] = [0x00, 0xFF, 0xFF, 0xFF];
pub const HEADER_LEN: usize = 10;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameStats {
    pub frames: u64,
    /// Frames abandoned because a new marker arrived before they were full.
    pub dropped: u64,
    pub bytes: u64,
}

pub struct FrameAssembler {
    buf: Vec<u8>,
    pos: usize,
    synced: bool,
    skip: usize,
    /// Last bytes of the previous payload, to catch a marker split across packets.
    tail: Vec<u8>,
    pub stats: FrameStats,
}

fn find_sync(data: &[u8]) -> Option<usize> {
    data.windows(4).position(|w| w == SYNC)
}

impl FrameAssembler {
    pub fn new(frame_bytes: usize) -> Self {
        Self {
            buf: vec![0; frame_bytes],
            pos: 0,
            synced: false,
            skip: 0,
            tail: Vec::with_capacity(3),
            stats: FrameStats::default(),
        }
    }

    pub fn frame_bytes(&self) -> usize {
        self.buf.len()
    }

    pub fn reset(&mut self) {
        self.pos = 0;
        self.synced = false;
        self.skip = 0;
        self.tail.clear();
    }

    /// Feed one packet payload. Calls `on_frame` with each completed frame.
    pub fn push(&mut self, packet: &[u8], mut on_frame: impl FnMut(&[u8])) {
        self.stats.bytes += packet.len() as u64;
        let mut rest = packet;

        // A marker split across earlier packets and this one.
        if !self.tail.is_empty() {
            let mut joined = [0u8; 6];
            let n = self.tail.len();
            joined[..n].copy_from_slice(&self.tail);
            let m = packet.len().min(3);
            joined[n..n + m].copy_from_slice(&packet[..m]);
            if let Some(i) = find_sync(&joined[..n + m]) {
                if i < n {
                    // The tail bytes from i onward were marker, not pixels.
                    self.retract(n - i);
                    self.start_frame();
                    rest = &packet[i + 4 - n..];
                }
            }
        }

        let prev_tail = core::mem::take(&mut self.tail);
        self.consume(rest, &mut on_frame);
        // Keep the last 3 bytes of the stream, which may span several tiny packets.
        let mut t = if rest.len() == packet.len() {
            prev_tail
        } else {
            Vec::new()
        };
        t.extend_from_slice(&rest[rest.len().saturating_sub(3)..]);
        self.tail = t.split_off(t.len().saturating_sub(3));
    }

    fn consume(&mut self, mut data: &[u8], on_frame: &mut impl FnMut(&[u8])) {
        while !data.is_empty() {
            if self.skip > 0 {
                let n = self.skip.min(data.len());
                self.skip -= n;
                data = &data[n..];
                continue;
            }
            match find_sync(data) {
                Some(i) => {
                    self.append(&data[..i], on_frame);
                    self.start_frame();
                    data = &data[i + 4..];
                }
                None => {
                    self.append(data, on_frame);
                    return;
                }
            }
        }
    }

    fn start_frame(&mut self) {
        if self.synced && self.pos > 0 {
            self.stats.dropped += 1;
        }
        self.synced = true;
        self.pos = 0;
        self.skip = HEADER_LEN;
    }

    fn append(&mut self, data: &[u8], on_frame: &mut impl FnMut(&[u8])) {
        if !self.synced || data.is_empty() {
            return;
        }
        let n = data.len().min(self.buf.len() - self.pos);
        self.buf[self.pos..self.pos + n].copy_from_slice(&data[..n]);
        self.pos += n;
        if self.pos == self.buf.len() {
            self.stats.frames += 1;
            on_frame(&self.buf);
            // Wait for the next marker before collecting again.
            self.synced = false;
            self.pos = 0;
        }
    }

    fn retract(&mut self, n: usize) {
        if self.synced {
            self.pos = self.pos.saturating_sub(n);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_stream(frame_bytes: usize, frames: u8) -> Vec<u8> {
        let mut s = vec![0x55; 7]; // junk before first marker
        for f in 0..frames {
            s.extend_from_slice(&SYNC);
            s.extend_from_slice(&[0xAA; HEADER_LEN]);
            s.extend((0..frame_bytes).map(|i| (i as u8).wrapping_add(f) | 0x01));
        }
        s
    }

    fn run(packet_len: usize) -> (Vec<Vec<u8>>, FrameStats) {
        let fb = 64;
        let stream = frame_stream(fb, 3);
        let mut a = FrameAssembler::new(fb);
        let mut out = Vec::new();
        for p in stream.chunks(packet_len) {
            a.push(p, |f| out.push(f.to_vec()));
        }
        (out, a.stats)
    }

    #[test]
    fn assembles_frames_for_any_packet_size() {
        for len in [1, 2, 3, 4, 5, 7, 13, 64, 100, 1000] {
            let (frames, stats) = run(len);
            assert_eq!(frames.len(), 3, "packet len {len}");
            assert_eq!(stats.dropped, 0, "packet len {len}");
            for (f, data) in frames.iter().enumerate() {
                let want: Vec<u8> = (0..64)
                    .map(|i| (i as u8).wrapping_add(f as u8) | 0x01)
                    .collect();
                assert_eq!(data, &want, "packet len {len} frame {f}");
            }
        }
    }

    #[test]
    fn short_frame_is_dropped() {
        let mut s = Vec::new();
        s.extend_from_slice(&SYNC);
        s.extend_from_slice(&[0; HEADER_LEN]);
        s.extend_from_slice(&[1; 10]); // truncated
        s.extend_from_slice(&SYNC);
        s.extend_from_slice(&[0; HEADER_LEN]);
        s.extend_from_slice(&[2; 16]);
        let mut a = FrameAssembler::new(16);
        let mut n = 0;
        a.push(&s, |f| {
            assert_eq!(f, &[2; 16]);
            n += 1;
        });
        assert_eq!((n, a.stats.dropped), (1, 1));
    }
}
