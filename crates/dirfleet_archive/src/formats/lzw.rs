//! Decoder for Unix `compress` (`.Z`) streams: adaptive LZW with codes of 9
//! up to 16 bits, packed least significant bit first in groups of eight
//! codes. Widening the codes or clearing the table discards the rest of the
//! current group, which is what sets this variant apart from GIF's LZW.

use std::io::{self, Read};

const CLEAR: u16 = 256;
const INIT_BITS: u32 = 9;

pub(crate) struct LzwReader<R> {
    inner: R,
    max_bits: u32,
    block_mode: bool,
    bits: u32,
    max_code: u32,
    next_code: u32,
    prefix: Vec<u16>,
    suffix: Vec<u8>,
    previous: Option<u16>,
    first_char: u8,
    /// Codes of the current group and how many of them are used.
    group: [u16; 8],
    group_len: usize,
    group_pos: usize,
    /// Decoded bytes, returned from `out_pos` on.
    out: Vec<u8>,
    out_pos: usize,
    /// One string, last byte first, while it is being decoded.
    stack: Vec<u8>,
    done: bool,
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

impl<R: Read> LzwReader<R> {
    /// Reads the three-byte header; fails unless `inner` starts a `.Z` stream.
    pub fn new(mut inner: R) -> io::Result<Self> {
        let mut header = [0u8; 3];
        inner.read_exact(&mut header)?;
        if header[..2] != [0x1f, 0x9d] {
            return Err(invalid("not a compress (.Z) stream"));
        }
        let max_bits = u32::from(header[2] & 0x1f);
        if !(INIT_BITS..=16).contains(&max_bits) {
            return Err(invalid("unsupported compress code size"));
        }
        let block_mode = header[2] & 0x80 != 0;
        let mut reader = Self {
            inner,
            max_bits,
            block_mode,
            bits: INIT_BITS,
            max_code: (1 << INIT_BITS) - 1,
            next_code: 0,
            prefix: vec![0; 1 << max_bits],
            suffix: vec![0; 1 << max_bits],
            previous: None,
            first_char: 0,
            group: [0; 8],
            group_len: 0,
            group_pos: 0,
            out: Vec::new(),
            out_pos: 0,
            stack: Vec::new(),
            done: false,
        };
        reader.reset_table();
        Ok(reader)
    }

    fn reset_table(&mut self) {
        self.next_code = if self.block_mode { 257 } else { 256 };
        self.bits = INIT_BITS;
        self.max_code = (1 << INIT_BITS) - 1;
        self.previous = None;
    }

    /// Reads the next group of up to eight codes of the current width.
    fn fill_group(&mut self) -> io::Result<bool> {
        let mut bytes = [0u8; 16];
        let want = self.bits as usize;
        let mut got = 0;
        while got < want {
            match self.inner.read(&mut bytes[got..want]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        let count = got * 8 / self.bits as usize;
        let mask = (1u32 << self.bits) - 1;
        for (i, code) in self.group.iter_mut().take(count).enumerate() {
            let bit = i * self.bits as usize;
            let mut word = 0u32;
            for (k, byte) in bytes[bit / 8..(bit / 8 + 3).min(16)].iter().enumerate() {
                word |= u32::from(*byte) << (8 * k);
            }
            *code = ((word >> (bit % 8)) & mask) as u16;
        }
        self.group_len = count;
        self.group_pos = 0;
        Ok(count > 0)
    }

    /// Decodes one code into `out`. Returns false at the end of input.
    fn step(&mut self) -> io::Result<bool> {
        if self.group_pos == self.group_len && !self.fill_group()? {
            return Ok(false);
        }
        let code = self.group[self.group_pos];
        self.group_pos += 1;

        if self.block_mode && code == CLEAR {
            self.reset_table();
            self.group_pos = self.group_len;
            return Ok(true);
        }
        let Some(previous) = self.previous else {
            if code > 255 {
                return Err(invalid("compress stream starts with an undefined code"));
            }
            self.first_char = code as u8;
            self.out.push(code as u8);
            self.previous = Some(code);
            return Ok(true);
        };

        self.stack.clear();
        let mut current = u32::from(code);
        if current >= self.next_code {
            // The code being defined: the previous string plus its first byte.
            if current > self.next_code {
                return Err(invalid("corrupt compress stream"));
            }
            self.stack.push(self.first_char);
            current = u32::from(previous);
        }
        while current > 255 {
            self.stack.push(self.suffix[current as usize]);
            current = u32::from(self.prefix[current as usize]);
        }
        self.first_char = current as u8;
        self.stack.push(self.first_char);
        self.out.extend(self.stack.iter().rev());

        if self.next_code < 1 << self.max_bits {
            self.prefix[self.next_code as usize] = previous;
            self.suffix[self.next_code as usize] = self.first_char;
            self.next_code += 1;
        }
        self.previous = Some(code);
        // Widen the codes as compress does: at the maximum width the limit
        // becomes the table size, which `next_code` never passes. With 9-bit
        // tables that leaves one widening to 10 bits after the table fills,
        // and the encoder does the same.
        if self.next_code > self.max_code {
            self.bits += 1;
            self.max_code = if self.bits == self.max_bits {
                1 << self.max_bits
            } else {
                (1 << self.bits) - 1
            };
            self.group_pos = self.group_len;
        }
        Ok(true)
    }
}

impl<R: Read> Read for LzwReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.out_pos == self.out.len() {
            self.out.clear();
            self.out_pos = 0;
            // Decode a batch, so each call returns more than one string.
            while self.out.len() < buf.len().max(4096) && !self.done {
                if !self.step()? {
                    self.done = true;
                }
            }
        }
        let n = buf.len().min(self.out.len() - self.out_pos);
        buf[..n].copy_from_slice(&self.out[self.out_pos..self.out_pos + n]);
        self.out_pos += n;
        Ok(n)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    /// Round trips through macOS's own `compress`.
    #[test]
    fn decodes_compress_output() {
        let mut data = Vec::new();
        for i in 0..200_000u32 {
            data.extend_from_slice(format!("line {} {}\n", i % 977, i * 7 % 13).as_bytes());
        }
        data.extend((0..300_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8));
        for bits in ["9", "12", "16"] {
            let mut child = Command::new("compress")
                .args(["-c", "-b", bits])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let mut stdin = child.stdin.take().unwrap();
            let input = data.clone();
            let writer = std::thread::spawn(move || {
                use std::io::Write;
                stdin.write_all(&input).unwrap();
            });
            let output = child.wait_with_output().unwrap();
            writer.join().unwrap();
            let mut decoded = Vec::new();
            LzwReader::new(&output.stdout[..])
                .unwrap()
                .read_to_end(&mut decoded)
                .unwrap();
            assert!(decoded == data, "{bits}-bit codes");
        }
    }
}
