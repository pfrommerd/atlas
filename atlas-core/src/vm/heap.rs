

// Term layout is similar to the HVM term layout.
// SUB (1 bit) | TAG (7 bits) | EXT (16 bits) | VAL (40 bits)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term(u64);

impl Term {
    pub fn is_sub(&self) -> bool {
        self.0 & (1 << 63) != 0 // SUB bit
    }
    pub fn tag(&self) -> Tag {
        let tag = (self.0 >> 56) as u8;
        unsafe {
            debug_assert!(tag < (Tag::Invalid as u8));
            std::mem::transmute(tag)
        }
    }
    pub unsafe fn tag_unchecked(&self) -> Tag {
        let tag = (self.0 >> 56) as u8;
        unsafe { std::mem::transmute(tag) }
    }
    pub fn ext(&self) -> u16 {
        (self.0 >> 40) as u16
    }
    pub fn val(&self) -> u64 {
        self.0 & 0xFFFFFFFFFFFF
    }
}

#[repr(i8)]
pub enum Tag {
    App,
    Var,
    Dp0,
    Dp1,
    Lam,
    Bjv,
    Bj0,
    Bj1,
    Sup,
    Dup,
    Ctr,
    Mat,
    Swi,
    Use,
    Bop,
    // Special short-circuit operators
    And,
    Or,
    Wld,
    Dsu,
    Ddu,
    Invalid,
}