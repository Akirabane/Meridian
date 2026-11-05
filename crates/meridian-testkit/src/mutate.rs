//! Hostile inputs for tests: a seeded random source that gives the same bytes on every
//! machine and every run, and changes to a valid input of the kinds a parser must
//! survive (a flipped bit, a byte set to a value at the edge of a range, bytes cut,
//! repeated or inserted, a length made huge or negative).
//!
//! No crate of the internet is needed: the generator is xorshift64*, enough to spread
//! the changes, and a failure prints its seed and its round, so that it can be run
//! again.

/// A seeded source of numbers (xorshift64*).
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    /// The source for `seed`. Seed 0 is taken as 1, which xorshift needs.
    pub fn new(seed: u64) -> Self {
        Rng(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// A number below `n`, or 0 when `n` is 0.
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }

    pub fn byte(&mut self) -> u8 {
        self.next_u64() as u8
    }

    /// `n` bytes.
    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.byte()).collect()
    }

    /// One of `items`.
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

/// Bytes at the edges of what a parser reads: a VarInt's continuation bit, a sign, a
/// length's limits.
const EDGES: [u8; 8] = [0x00, 0x01, 0x7f, 0x80, 0xfe, 0xff, b'\n', b'\r'];

/// VarInts a length field may be changed to: zero, negative, just past the limits a
/// reader holds, the largest, and one that never ends.
const LENGTHS: [&[u8]; 7] = [
    &[0x00],
    &[0xff, 0xff, 0xff, 0xff, 0x0f],
    &[0x81, 0x10],
    &[0xff, 0xff, 0x7f],
    &[0xff, 0xff, 0xff, 0xff, 0x07],
    &[0x80, 0x80, 0x80, 0x80, 0x80, 0x01],
    &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
];

/// `input`, with one to four changes.
pub fn mutate(rng: &mut Rng, input: &[u8]) -> Vec<u8> {
    let mut out = input.to_vec();
    for _ in 0..=rng.below(4) {
        let at = rng.below(out.len() + 1);
        match rng.below(8) {
            // A bit.
            0 if !out.is_empty() => {
                let i = rng.below(out.len());
                out[i] ^= 1 << rng.below(8);
            }
            // A byte at an edge.
            1 if !out.is_empty() => {
                let i = rng.below(out.len());
                out[i] = *rng.pick(&EDGES);
            }
            // A random byte.
            2 if !out.is_empty() => {
                let i = rng.below(out.len());
                out[i] = rng.byte();
            }
            // Cut short.
            3 => out.truncate(at),
            // A piece taken out.
            4 => {
                let end = (at + 1 + rng.below(8)).min(out.len());
                out.drain(at.min(end)..end);
            }
            // A piece repeated.
            5 => {
                let end = (at + 1 + rng.below(16)).min(out.len());
                let piece = out[at.min(end)..end].to_vec();
                for (k, b) in piece.into_iter().enumerate() {
                    out.insert(at.min(out.len()) + k, b);
                }
            }
            // Random bytes inserted.
            6 => {
                let n = 1 + rng.below(16);
                let bytes = rng.bytes(n);
                for (k, b) in bytes.into_iter().enumerate() {
                    out.insert(at + k, b);
                }
            }
            // A length made extreme, where a VarInt might be.
            _ => {
                let length = rng.pick(&LENGTHS);
                let end = (at + 1).min(out.len());
                out.splice(at.min(end)..end, length.iter().copied());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_gives_the_same_changes() {
        let input = b"a valid input of some length".to_vec();
        let (mut a, mut b) = (Rng::new(7), Rng::new(7));
        for _ in 0..100 {
            assert_eq!(mutate(&mut a, &input), mutate(&mut b, &input));
        }
        let mut c = Rng::new(8);
        let differ = (0..100).any(|_| mutate(&mut a, &input) != mutate(&mut c, &input));
        assert!(differ);
    }

    #[test]
    fn changes_stay_small_and_take_any_input() {
        let mut rng = Rng::new(1);
        for len in 0..64 {
            let input = rng.bytes(len);
            for _ in 0..50 {
                let out = mutate(&mut rng, &input);
                assert!(out.len() <= input.len() + 4 * 16 + 24, "{}", out.len());
            }
        }
    }
}
