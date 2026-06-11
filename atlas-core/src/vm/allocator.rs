use crate::util::U56;
use crate::vm::term::Node;

use sharded_slab::{Config as SlabConfig, DefaultConfig as DefaultSlabConfig, Slab};

struct AllocConfig;
impl SlabConfig for AllocConfig {
    const RESERVED_BITS: usize = (64 - U56::BITS as usize) + 4;
    const INITIAL_PAGE_SIZE: usize = DefaultSlabConfig::INITIAL_PAGE_SIZE;
    const MAX_THREADS: usize = DefaultSlabConfig::MAX_THREADS;
    const MAX_PAGES: usize = DefaultSlabConfig::MAX_PAGES;
}

// Represents a pointer to an allocated node or a duplicate cell.
// The top 4 bits are used to encode the allocation type.
#[repr(transparent)]
pub struct Addr(U56);

impl Addr {
    pub unsafe fn new_unchecked(value: U56) -> Self {
        Addr(value)
    }
    // SAFETY: The caller must ensure that addr < 2^52
    pub(crate) unsafe fn new_of_type(type: AllocType, index: usize) -> Self {
        debug_assert!(index < 2^52);
        let value = (index as u64) | ((type as u64) << 52);
        unsafe { Addr::new_unchecked(U56::new_unchecked(value)) }
    }
    pub fn index(&self) -> u64 {
        self.0
    }
}

// The allocation type is encoded in the top 4 bits of the address
#[repr(u64)]
enum AllocType {
    Dup = 0,
    Single = 1,
    Pair = 2,
    Triple = 3,
    Quad = 4,
    Oct = 5,
    Sixteen = 6,
    ThirtyTwo = 7,
    SixtyFour = 8,
    OneTwentyEight = 9,
    TwoFiftySix = 10,
}

pub struct Allocator {
    // for dups, a special table
    dup: Slab<DupSlot>,
    // We allow allocations of up to 256 cells.
    single: Slab<[Node; 1]>,
    pair: Slab<[Node; 2]>,
    triple: Slab<[Node; 3]>,
    quad: Slab<[Node; 4]>,
    oct: Slab<[Node; 8]>,
    sixteen: Slab<[Node; 16]>,
    thirty_two: Slab<[Node; 32]>,
    sixty_four: Slab<[Node; 64]>,
    one_twenty_eight: Slab<[Node; 128]>,
    two_fifty_six: Slab<[Node; 256]>,
}

impl Allocator {
    pub fn new() -> Self {
        Allocator {
            dup: Slab::new(),
            single: Slab::new(),
            pair: Slab::new(),
            triple: Slab::new(),
            quad: Slab::new(),
            oct: Slab::new(),
            sixteen: Slab::new(),
            thirty_two: Slab::new(),
            sixty_four: Slab::new(),
            one_twenty_eight: Slab::new(),
            two_fifty_six: Slab::new(),
        }
    }
    fn split_addr(&self, addr: Addr) -> (AllocType, usize) {}
    pub fn alloc_node(&self, value: Node) -> Addr {
        let slot = self.single.insert([value]).unwrap() as u64;
        unsafe { Addr::new_unchecked(slot | SINGLE_MASK) }
    }
}
