// Stores a 56-bit unsigned integer in a 64-bit unsigned integer.
// The top 8 bits are always zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct U56(u64);

impl U56 {
    pub const MASK: u64 = (1 << 56) - 1;
    pub const fn new(value: u64) -> Self {
        U56(value & !Self::MASK)
    }
    pub const unsafe fn new_unchecked(value: u64) -> Self {
        U56(value)
    }
    pub fn to_u64(&self) -> u64 {
        self.0
    }
}

impl From<u32> for U56 {
    fn from(value: u32) -> Self {
        U56::new(value as u64)
    }
}
