//! KISS framing, as spoken by Direwolf and hardware TNCs. Receive side only.

const FEND: u8 = 0xC0;
const FESC: u8 = 0xDB;
const TFEND: u8 = 0xDC;
const TFESC: u8 = 0xDD;

/// Frames longer than this are line noise, not AX.25.
const MAX_FRAME: usize = 1024;

/// Streaming decoder: push bytes as they arrive, get complete data frames out.
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
    escaped: bool,
    /// Seen the first FEND; bytes before it are the tail of some frame.
    synced: bool,
    /// The current frame exceeded `MAX_FRAME` and will be dropped.
    overflow: bool,
}

impl Decoder {
    /// Feed bytes; calls `on_frame` with the AX.25 payload of each completed
    /// data frame (KISS command byte stripped).
    pub fn push(&mut self, bytes: &[u8], mut on_frame: impl FnMut(&[u8])) {
        for &b in bytes {
            if !self.synced {
                self.synced = b == FEND;
                continue;
            }
            match (b, self.escaped) {
                (FEND, _) => {
                    // Command byte: high nibble is the TNC port, low nibble 0 = data.
                    if let [cmd, payload @ ..] = &self.buf[..] {
                        if cmd & 0x0F == 0 && !payload.is_empty() && !self.overflow {
                            on_frame(payload);
                        }
                    }
                    self.buf.clear();
                    self.escaped = false;
                    self.overflow = false;
                }
                (FESC, false) => self.escaped = true,
                (TFEND, true) => self.put(FEND),
                (TFESC, true) => self.put(FESC),
                (b, _) => self.put(b),
            }
        }
    }

    fn put(&mut self, b: u8) {
        self.escaped = false;
        if self.buf.len() < MAX_FRAME {
            self.buf.push(b);
        } else {
            self.overflow = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(chunks: &[&[u8]]) -> Vec<Vec<u8>> {
        let mut d = Decoder::default();
        let mut out = Vec::new();
        for c in chunks {
            d.push(c, |f| out.push(f.to_vec()));
        }
        out
    }

    #[test]
    fn decodes_frame_split_across_reads() {
        let got = frames(&[&[FEND, 0x00, b'a'], &[b'b', FEND]]);
        assert_eq!(got, [b"ab".to_vec()]);
    }

    #[test]
    fn unescapes() {
        let got = frames(&[&[FEND, 0x00, FESC, TFEND, FESC, TFESC, FEND]]);
        assert_eq!(got, [vec![FEND, FESC]]);
    }

    #[test]
    fn skips_empty_and_non_data_frames() {
        let got = frames(&[&[FEND, FEND, 0x06, 1, FEND, 0x10, b'x', FEND]]);
        assert_eq!(got, [b"x".to_vec()]); // port 1 data frame is still data
    }

    #[test]
    fn bytes_before_the_first_fend_are_discarded() {
        // 0x30 has a zero low nibble, so the tail would otherwise pass as a data frame.
        let got = frames(&[&[0x30, b'j', b'u', b'n', b'k', FEND, 0x00, b'x', FEND]]);
        assert_eq!(got, [b"x".to_vec()]);
    }

    #[test]
    fn overlong_frame_is_dropped_not_truncated() {
        let mut long = vec![FEND, 0x00];
        long.extend(std::iter::repeat_n(b'z', MAX_FRAME + 1));
        long.extend([FEND, 0x00, b'x', FEND]);
        assert_eq!(frames(&[&long]), [b"x".to_vec()]);
    }
}
