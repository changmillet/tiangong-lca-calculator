//! SP800-90A rev1 §10.1.2 `HMAC_DRBG` core, used solely for reproducible sampling.
use anyhow::ensure;
use hmac::{Hmac, Mac};
use sha2::Sha256;
fn mac(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts every key length");
    for part in parts {
        h.update(part);
    }
    h.finalize().into_bytes().into()
}
pub(super) struct Drbg {
    k: [u8; 32],
    v: [u8; 32],
}
impl Drbg {
    pub(super) fn instantiate(entropy: &[u8], nonce: &[u8], personalization: &[u8]) -> Self {
        let mut state = Self {
            k: [0; 32],
            v: [1; 32],
        };
        state.update(&[entropy, nonce, personalization].concat());
        state
    }
    fn update(&mut self, data: &[u8]) {
        self.k = mac(&self.k, &[&self.v, &[0], data]);
        self.v = mac(&self.k, &[&self.v]);
        if !data.is_empty() {
            self.k = mac(&self.k, &[&self.v, &[1], data]);
            self.v = mac(&self.k, &[&self.v]);
        }
    }
    pub(super) fn generate(&mut self, n: usize) -> Vec<u8> {
        let mut result = Vec::with_capacity(n);
        while result.len() < n {
            self.v = mac(&self.k, &[&self.v]);
            result.extend_from_slice(&self.v[..(n - result.len()).min(32)]);
        }
        self.update(&[]);
        result
    }
    pub(super) fn word(&mut self) -> u64 {
        u64::from_be_bytes(self.generate(8).try_into().expect("eight bytes"))
    }
    // The shifted word is at most 2^53-1 and therefore exactly representable.
    #[allow(clippy::cast_precision_loss)]
    pub(super) fn uniform(&mut self) -> f64 {
        (self.word() >> 11) as f64 / 9_007_199_254_740_992.0
    }
    pub(super) fn row(&mut self, n: usize) -> anyhow::Result<usize> {
        select_row(n, || self.word())
    }
}
pub(super) fn select_row(n: usize, mut word: impl FnMut() -> u64) -> anyhow::Result<usize> {
    ensure!(n > 0, "study_advanced_empirical_rows");
    let n = u64::try_from(n)?;
    let threshold = u64::try_from((u128::from(u64::MAX) + 1) % u128::from(n))?;
    loop {
        let x = word();
        if x >= threshold {
            return Ok(usize::try_from(x % n)?);
        }
    }
}
