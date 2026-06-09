// Stores a 56-bit unsigned integer in a 64-bit unsigned integer.
// The upper 8 bits may not be zero, but when converting to a u64,
// they will be zeroed out.
pub struct U56(u64);
