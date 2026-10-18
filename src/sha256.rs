//! SHA-256 (FIPS 180-4), written out so no crypto crate is needed. Checked against the NIST example vectors.
//! Used for `hash`, `dupes` and `find --hash` (copy/move verification compares bytes, content.rs). Streaming: a
//! file of any size, 1 MiB at a time.
//!
//! Two block functions compute the same thing:
//!   portable  plain Rust, runs on every CPU (the 2011 MacBook Air, the VAIO's i7-3632QM, ARM).
//!   sha-ni    the x86-64 SHA extensions (Intel since Goldmont / Ice Lake / Alder Lake, AMD since Zen): the CPU does
//!             two rounds per instruction. Chosen at RUN time, not build time, so one binary is right on both an
//!             old CPU without the instructions and a new one with them (plan section 3, item 3).
//! The choice is made once per process and reported (`hash --json` says `sha256_impl`, `profile` says it too), so
//! a speed claim can always be traced to the code path that produced it. The tests run the NIST vectors through
//! both functions and compare them on random inputs; a disagreement fails the build.

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// Which block function a hasher uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Impl {
    Portable,
    /// only ever constructed after the CPU said it has the SHA extensions (see `detect`)
    ShaNi,
}

impl Impl {
    pub fn name(self) -> &'static str {
        match self {
            Impl::Portable => "portable",
            Impl::ShaNi => "sha-ni",
        }
    }
}

/// The fastest block function this CPU can run, asked once per process (CPUID is cheap, but not free per file).
pub fn detect() -> Impl {
    use std::sync::OnceLock;
    static CHOSEN: OnceLock<Impl> = OnceLock::new();
    *CHOSEN.get_or_init(|| {
        #[cfg(target_arch = "x86_64")]
        {
            // all four: the SHA instructions themselves, and the SSE shuffles/blends the code around them uses
            if std::is_x86_feature_detected!("sha") && std::is_x86_feature_detected!("sse2")
                && std::is_x86_feature_detected!("ssse3") && std::is_x86_feature_detected!("sse4.1") {
                return Impl::ShaNi;
            }
        }
        Impl::Portable
    })
}

/// "sha-ni" or "portable": what `Sha256::default()` uses on this machine.
pub fn implementation() -> &'static str {
    detect().name()
}

pub struct Sha256 {
    h: [u32; 8],
    buf: [u8; 64],
    n: usize,
    len: u64,
    imp: Impl,
}

impl Default for Sha256 {
    /// The fastest correct path for this CPU.
    fn default() -> Self {
        Self::with(detect())
    }
}

impl Sha256 {
    /// The plain-Rust path whatever the CPU has: the reference the fast path is tested against.
    pub fn portable() -> Self {
        Self::with(Impl::Portable)
    }

    fn with(imp: Impl) -> Self {
        Sha256 {
            h: [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19],
            buf: [0; 64],
            n: 0,
            len: 0,
            imp,
        }
    }

    pub fn implementation(&self) -> Impl {
        self.imp
    }

    /// Compress whole 64-byte blocks (`data.len()` is a multiple of 64) with the chosen function.
    fn blocks(&mut self, data: &[u8]) {
        debug_assert!(data.len() % 64 == 0);
        match self.imp {
            #[cfg(target_arch = "x86_64")]
            // SAFETY: Impl::ShaNi is only produced by detect(), after CPUID reported sha, sse2, ssse3 and sse4.1.
            Impl::ShaNi => unsafe { ni::compress(&mut self.h, data) },
            _ => {
                for b in data.chunks_exact(64) {
                    Self::block(&mut self.h, b);
                }
            }
        }
    }

    fn block(h: &mut [u32; 8], b: &[u8]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut bb, mut c, mut d, mut e, mut f, mut g, mut hh] = *h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & bb) ^ (a & c) ^ (bb & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = bb;
            bb = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, bb, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.len += data.len() as u64;
        if self.n > 0 {
            let take = (64 - self.n).min(data.len());
            self.buf[self.n..self.n + take].copy_from_slice(&data[..take]);
            self.n += take;
            data = &data[take..];
            if self.n < 64 {
                return; // the buffer is not full yet: keep what is in it
            }
            let b = self.buf;
            self.blocks(&b);
            self.n = 0;
        }
        // every whole block in one call: the SHA-NI path keeps the state in registers across them
        let whole = data.len() / 64 * 64;
        if whole > 0 {
            self.blocks(&data[..whole]);
            data = &data[whole..];
        }
        self.buf[..data.len()].copy_from_slice(data);
        self.n = data.len();
    }

    pub fn finish(mut self) -> [u8; 32] {
        let bits = self.len.wrapping_mul(8);
        self.update(&[0x80]);
        while self.n != 56 {
            self.update(&[0]);
        }
        self.update(&bits.to_be_bytes());
        let mut out = [0u8; 32];
        for (i, v) in self.h.iter().enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&v.to_be_bytes());
        }
        out
    }
}

pub fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// SHA-256 of a file, streamed; `limit`: only the first N bytes (for the quick dupes pass).
pub fn file(path: &std::path::Path, limit: Option<u64>) -> std::io::Result<[u8; 32]> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::default();
    let mut buf = vec![0u8; 1 << 20];
    let mut left = limit.unwrap_or(u64::MAX);
    while left > 0 {
        let want = (buf.len() as u64).min(left) as usize;
        let n = f.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        left -= n as u64;
    }
    Ok(h.finish())
}

/// The SHA extensions path. Layout follows Intel's white paper "Intel SHA Extensions" (2013) and its reference code:
/// the eight state words live in two registers as ABEF and CDGH, `sha256rnds2` does two rounds, `sha256msg1/2`
/// compute the message schedule four words at a time. Only reached through `Sha256::blocks` after `detect()`.
#[cfg(target_arch = "x86_64")]
mod ni {
    use std::arch::x86_64::*;

    /// One message-schedule step: W[t..t+4] from the four previous groups (FIPS 180-4 section 6.2.2 step 1).
    #[inline]
    #[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
    unsafe fn schedule(w0: __m128i, w1: __m128i, w2: __m128i, w3: __m128i) -> __m128i {
        let t = _mm_add_epi32(_mm_sha256msg1_epu32(w0, w1), _mm_alignr_epi8(w3, w2, 4));
        _mm_sha256msg2_epu32(t, w3)
    }

    /// Four rounds: add the round constants K[4i..4i+4] to the message words, two rounds per instruction.
    #[inline]
    #[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
    unsafe fn rounds4(abef: &mut __m128i, cdgh: &mut __m128i, w: __m128i, i: usize) {
        let k = _mm_loadu_si128(super::K.as_ptr().add(4 * i) as *const __m128i);
        let wk = _mm_add_epi32(w, k);
        *cdgh = _mm_sha256rnds2_epu32(*cdgh, *abef, wk);
        // the instruction takes the next two words from the LOW half; move the high half down
        *abef = _mm_sha256rnds2_epu32(*abef, *cdgh, _mm_shuffle_epi32(wk, 0x0E));
    }

    /// Compress every 64-byte block of `data` into `h`.
    ///
    /// # Safety
    /// The CPU must have sha, sse2, ssse3 and sse4.1 (checked by `detect`); `data.len()` must be a multiple of 64.
    #[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
    pub unsafe fn compress(h: &mut [u32; 8], data: &[u8]) {
        // SHA-256 reads its message as big-endian words; this shuffle swaps the bytes of each 32-bit lane
        let bswap = _mm_set_epi64x(0x0c0d_0e0f_0809_0a0b, 0x0405_0607_0001_0203);
        // state words a..h -> the two registers the instructions want: ABEF and CDGH (highest lane first)
        let dcba = _mm_loadu_si128(h.as_ptr() as *const __m128i);
        let hgfe = _mm_loadu_si128(h.as_ptr().add(4) as *const __m128i);
        let cdab = _mm_shuffle_epi32(dcba, 0xB1);
        let efgh = _mm_shuffle_epi32(hgfe, 0x1B);
        let mut abef = _mm_alignr_epi8(cdab, efgh, 8);
        let mut cdgh = _mm_blend_epi16(efgh, cdab, 0xF0);
        for block in data.chunks_exact(64) {
            let (abef0, cdgh0) = (abef, cdgh);
            let p = block.as_ptr() as *const __m128i;
            // w[j] holds message group j mod 4: a ring of four registers, the newest replacing the oldest
            let mut w = [
                _mm_shuffle_epi8(_mm_loadu_si128(p), bswap),
                _mm_shuffle_epi8(_mm_loadu_si128(p.add(1)), bswap),
                _mm_shuffle_epi8(_mm_loadu_si128(p.add(2)), bswap),
                _mm_shuffle_epi8(_mm_loadu_si128(p.add(3)), bswap),
            ];
            for (i, wi) in w.iter().enumerate() {
                rounds4(&mut abef, &mut cdgh, *wi, i);
            }
            for i in 4..16 {
                let next = schedule(w[i % 4], w[(i + 1) % 4], w[(i + 2) % 4], w[(i + 3) % 4]);
                w[i % 4] = next;
                rounds4(&mut abef, &mut cdgh, next, i);
            }
            abef = _mm_add_epi32(abef, abef0);
            cdgh = _mm_add_epi32(cdgh, cdgh0);
        }
        // and back to a..h
        let feba = _mm_shuffle_epi32(abef, 0x1B);
        let dchg = _mm_shuffle_epi32(cdgh, 0xB1);
        _mm_storeu_si128(h.as_mut_ptr() as *mut __m128i, _mm_blend_epi16(feba, dchg, 0xF0));
        _mm_storeu_si128(h.as_mut_ptr().add(4) as *mut __m128i, _mm_alignr_epi8(dchg, feba, 8));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(mut h: Sha256, data: &[u8]) -> String {
        h.update(data);
        hex(&h.finish())
    }

    /// the dispatching path: SHA-NI on a CPU that has it
    fn s(data: &[u8]) -> String {
        with(Sha256::default(), data)
    }

    /// the plain-Rust path, whatever the CPU
    fn p(data: &[u8]) -> String {
        with(Sha256::portable(), data)
    }

    const NIST: [(&[u8], &str); 3] = [
        (b"", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
        (b"abc", "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
        (b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
         "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"),
    ];
    const MILLION_A: &str = "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0";

    #[test]
    fn nist_vectors_through_both_paths() {
        for (msg, want) in NIST {
            assert_eq!(p(msg), want, "portable");
            assert_eq!(s(msg), want, "{}", implementation());
        }
    }

    #[test]
    fn a_million_a_through_both_paths_in_odd_chunks_equals_one_call() {
        let data = vec![b'a'; 1_000_000];
        for make in [Sha256::portable as fn() -> Sha256, Sha256::default] {
            assert_eq!(with(make(), &data), MILLION_A);
            let mut h = make();
            for c in data.chunks(997) {
                h.update(c);
            }
            assert_eq!(hex(&h.finish()), MILLION_A, "{:?}", make().implementation());
        }
    }

    /// Every length from 0 to 300 crosses the 55/56/64-byte padding edges several times; random bytes from a
    /// fixed-seed generator (xorshift), so a failure is reproducible.
    #[test]
    fn random_buffers_agree_between_paths() {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        for len in 0..=300usize {
            let data: Vec<u8> = (0..len).map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            }).collect();
            assert_eq!(s(&data), p(&data), "length {len}");
        }
    }

    /// 2026-10-18: the fast path must really be taken where it exists, or the speed claim is false. On a CPU that
    /// reports the SHA extensions the default hasher is SHA-NI; elsewhere portable (and this says so).
    #[test]
    fn the_dispatcher_uses_sha_ni_when_the_cpu_has_it() {
        #[cfg(target_arch = "x86_64")]
        let has = std::is_x86_feature_detected!("sha") && std::is_x86_feature_detected!("sse4.1");
        #[cfg(not(target_arch = "x86_64"))]
        let has = false;
        let want = if has { Impl::ShaNi } else { Impl::Portable };
        assert_eq!(Sha256::default().implementation(), want);
        assert_eq!(implementation(), want.name());
        if !has {
            eprintln!("NOTE: this CPU has no SHA extensions; only the portable path was tested");
        }
    }

    /// Speed of both paths on 256 MiB held in memory (no disk in the measurement). Run by hand:
    /// cargo test --release --offline --locked --lib sha256 -- --ignored --nocapture
    #[test]
    #[ignore]
    fn speed_of_both_paths_on_256_mib() {
        let data = vec![0x5Au8; 256 << 20];
        let mut rows = Vec::new();
        for (name, make) in [("portable", Sha256::portable as fn() -> Sha256), ("dispatch", Sha256::default)] {
            let mut best = f64::MAX;
            let mut digest = String::new();
            for _ in 0..3 {
                let t = std::time::Instant::now();
                digest = with(make(), &data);
                best = best.min(t.elapsed().as_secs_f64());
            }
            rows.push((name, make().implementation().name(), best, digest));
        }
        for (name, imp, secs, _) in &rows {
            eprintln!("{name:<9} ({imp:<8}) best of 3: {secs:.3} s = {:.0} MiB/s", 256.0 / secs);
        }
        assert_eq!(rows[0].3, rows[1].3, "both paths give the same digest");
    }
}
