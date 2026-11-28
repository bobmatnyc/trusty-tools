//! SHA-256, for the dashboard change log's per-document hash.
//!
//! Why: each `changes.jsonl` line carries the SHA-256 of the bytes it wrote,
//! as `dashboard_jsonl.py` does with `hashlib.sha256(line).hexdigest()`, so
//! `fsck` can tell a hand-edited document from a logged one (#10063 PR 2).
//! The crate's leaf dependency rule (std, serde, thiserror, tracing, libc)
//! leaves no hash crate, so it is written here, as `sha1.rs` is. It is a
//! state fingerprint, never a security boundary.
//! What: FIPS 180-4 SHA-256 over a byte slice, as lowercase hex, and the
//! incremental [`Sha256`] it is built on.
//! Test: `sha256_known_vectors`, `sha256_streamed_matches_one_shot`.

const K: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

/// Lowercase hex SHA-256 digest of `data`.
pub(crate) fn sha256_hex(data: &[u8]) -> String {
    let mut s = Sha256::new();
    s.update(data);
    s.hex()
}

/// Why: an upload body is hashed while it streams to disk, so memory per
/// upload stays one block, not the body's size (#12029 P2h).
/// What: FIPS 180-4 state fed by [`Sha256::update`] in any split; [`Sha256::hex`]
/// pads and answers the same digest [`sha256_hex`] gives for the joined bytes.
/// Test: `sha256_streamed_matches_one_shot`.
pub(crate) struct Sha256 {
    h: [u32; 8],
    buf: [u8; 64],
    fill: usize,
    len: u64,
}

impl Sha256 {
    /// The initial state.
    pub(crate) fn new() -> Self {
        Self {
            h: [
                0x6a09_e667,
                0xbb67_ae85,
                0x3c6e_f372,
                0xa54f_f53a,
                0x510e_527f,
                0x9b05_688c,
                0x1f83_d9ab,
                0x5be0_cd19,
            ],
            buf: [0; 64],
            fill: 0,
            len: 0,
        }
    }

    /// Feeds `data`; any split of a message gives the same digest.
    pub(crate) fn update(&mut self, data: &[u8]) {
        self.len = self.len.wrapping_add(data.len() as u64);
        self.absorb(data);
    }

    fn absorb(&mut self, mut data: &[u8]) {
        if self.fill > 0 {
            let take = (64 - self.fill).min(data.len());
            self.buf[self.fill..self.fill + take].copy_from_slice(&data[..take]);
            self.fill += take;
            data = &data[take..];
            if self.fill < 64 {
                return;
            }
            let block = self.buf;
            compress(&mut self.h, &block);
            self.fill = 0;
        }
        let (blocks, rest) = data.as_chunks::<64>();
        for block in blocks {
            compress(&mut self.h, block);
        }
        self.buf[..rest.len()].copy_from_slice(rest);
        self.fill = rest.len();
    }

    /// Pads, finishes and answers the lowercase hex digest.
    pub(crate) fn hex(mut self) -> String {
        let bit_len = self.len.wrapping_mul(8);
        // 0x80, then zeros to 56 mod 64, then the 8-byte length; `fill` is 0..=63.
        let zeros = (64 + 55 - self.fill) % 64;
        let mut pad = vec![0x80u8];
        pad.resize(1 + zeros, 0);
        pad.extend_from_slice(&bit_len.to_be_bytes());
        self.absorb(&pad);
        debug_assert_eq!(self.fill, 0, "padding ends on a block boundary");
        self.h.iter().map(|v| format!("{v:08x}")).collect()
    }
}

/// One 64-byte block into the state.
fn compress(h: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for (i, word) in block.as_chunks::<4>().0.iter().enumerate() {
        w[i] = u32::from_be_bytes(*word);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = *h;
    for (ki, wi) in K.iter().zip(w) {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ (!e & g);
        let t1 = hh
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(*ki)
            .wrapping_add(wi);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        hh = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (hv, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
        *hv = hv.wrapping_add(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vectors() {
        // FIPS 180-4 / NIST CAVS examples.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let long = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
        assert_eq!(
            sha256_hex(long),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            sha256_hex(&vec![b'a'; 1_000_000]),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
        // 55 and 56 bytes straddle the one-block padding boundary.
        assert_eq!(
            sha256_hex(&[b'a'; 55]),
            "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318"
        );
        assert_eq!(
            sha256_hex(&[b'a'; 56]),
            "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"
        );
        // A dashboard line, against `hashlib.sha256(line).hexdigest()`.
        let line = "{\"data\":{\"t\":\"é\"},\"id\":\"A01\",\"sv\":1,\"version\":1}\n";
        assert_eq!(sha256_hex(line.as_bytes()), PY_LINE);
    }

    /// Every split of a message, across the block and padding boundaries,
    /// gives the one-shot digest.
    #[test]
    fn sha256_streamed_matches_one_shot() {
        let data: Vec<u8> = (0..300u32).map(|i| (i * 7 % 251) as u8).collect();
        for len in [0, 1, 55, 56, 63, 64, 65, 119, 120, 128, 300] {
            let want = sha256_hex(&data[..len]);
            for step in [1, 3, 55, 63, 64, 65, 200] {
                let mut s = Sha256::new();
                for part in data[..len].chunks(step) {
                    s.update(part);
                }
                assert_eq!(s.hex(), want, "len {len} step {step}");
            }
        }
        let mut s = Sha256::new();
        s.update(b"ab");
        s.update(b"");
        s.update(b"c");
        assert_eq!(
            s.hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// `python3 -c 'import hashlib;print(hashlib.sha256(<line>.encode()).hexdigest())'`.
    const PY_LINE: &str = "f5ea97a84bfe467bc38b069c95abf64a08e332ecc92683f34102af6adcf417eb";
}
