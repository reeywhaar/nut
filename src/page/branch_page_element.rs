use crate::consts::PGID;
use crate::utils::is_aligned;

/// BranchPageElement represents a node on a branch page.
#[derive(Debug)]
#[repr(C)]
pub struct BranchPageElement {
    pub pos: u32,
    pub ksize: u32,
    pub pgid: PGID,
}

impl BranchPageElement {
    /// binary struct's size (16)
    pub const SIZE: usize = size_of::<BranchPageElement>();

    /// key returns a byte slice of the node key.
    pub fn key(&self) -> &[u8] {
        debug_assert!(is_aligned(self));
        unsafe {
            std::slice::from_raw_parts(
                (self as *const Self).cast::<u8>().add(self.pos as usize),
                self.ksize as usize,
            )
        }
    }
}
