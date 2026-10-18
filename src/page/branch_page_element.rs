use crate::consts::PGID;

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
}
