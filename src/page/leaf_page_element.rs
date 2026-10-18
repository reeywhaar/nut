/// LeafPageElement represents a node on a leaf page.
#[derive(Debug)]
#[repr(C)]
pub struct LeafPageElement {
    pub flags: u32,
    pub pos: u32,
    pub ksize: u32,
    pub vsize: u32,
}

impl LeafPageElement {
    /// binary struct's size (16)
    pub const SIZE: usize = size_of::<LeafPageElement>();
}
