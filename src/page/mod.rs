#[cfg(test)]
pub mod page_data_tests;
#[cfg(test)]
pub mod tests;

use std::borrow::{Borrow, BorrowMut};
use std::fmt;
use std::marker::PhantomData;
use std::mem::offset_of;
use std::ops::{Deref, DerefMut};

mod branch_page_element;
mod freed_or_page_info;
mod leaf_page_element;
mod page_data;
mod page_info;

use crate::consts::{Flags, PGID};
use crate::errors::Error;
use crate::meta::Meta;

pub(crate) use branch_page_element::BranchPageElement;
pub use freed_or_page_info::FreedOrPageInfo;
pub(crate) use leaf_page_element::LeafPageElement;
pub(crate) use page_data::PageData;
pub(crate) use page_info::PageInfo;

type PageSize = usize;
type PageSpan = u32;

#[repr(C)]
pub(crate) struct Page {
    pub id: PGID,
    /// page type
    pub flags: Flags,
    /// number of items in page
    pub count: u16,
    /// span of the page.
    /// 0 means that page allocated in one page_size block, 1 — two and so on
    pub overflow: PageSpan,
    /// pointer to data, not used as actual u8
    ptr: PhantomData<u8>,
}

impl fmt::Debug for Page {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Page")
            .field("id", &self.id)
            .field("flags", &self.flags)
            .field("count", &self.count)
            .field("overflow", &self.overflow)
            .field("ptr", &(&raw const self.ptr))
            .finish()
    }
}

impl Page {
    pub(crate) fn header_size() -> usize {
        offset_of!(Page, ptr)
    }

    pub(crate) fn from_buf(buf: &[u8]) -> &Page {
        unsafe { &*buf.as_ptr().cast::<Page>() }
    }

    pub(crate) fn from_buf_mut(buf: &mut [u8]) -> &mut Page {
        unsafe { &mut *buf.as_mut_ptr().cast::<Page>() }
    }

    /// Returns pointer to page structure
    #[inline]
    pub(crate) fn get_data_ptr(&self) -> *const u8 {
        (&raw const self.ptr).cast::<u8>()
    }

    /// Returns pointer to page structure
    #[inline]
    pub(crate) fn get_data_mut_ptr(&mut self) -> *mut u8 {
        (&raw mut self.ptr).cast::<u8>()
    }

    pub(crate) fn data(&self) -> Result<PageData<'_>, Error> {
        let count = self.count as usize;
        unsafe {
            Ok(match self.flags {
                Flags::BRANCHES => PageData::Branches(std::slice::from_raw_parts(
                    self.get_data_ptr().cast::<BranchPageElement>(),
                    count,
                )),
                Flags::LEAVES => PageData::Leaves(std::slice::from_raw_parts(
                    self.get_data_ptr().cast::<LeafPageElement>(),
                    count,
                )),
                Flags::META => PageData::Meta(&*self.get_data_ptr().cast::<Meta>()),
                Flags::FREELIST => {
                    let ptr = self.get_data_ptr().cast::<PGID>();
                    let len = if count == 0xFFFF {
                        1 + *ptr as usize
                    } else {
                        count
                    };
                    PageData::Freelist(std::slice::from_raw_parts(ptr, len))
                }
                _ => return Err(format!("Unknown page type: {}", self.flags).into()),
            })
        }
    }

    #[expect(dead_code)]
    pub(crate) fn data_mut(&mut self) -> Result<PageData<'_>, Error> {
        let count = self.count as usize;
        unsafe {
            Ok(match self.flags {
                Flags::BRANCHES => PageData::Branches(std::slice::from_raw_parts(
                    self.get_data_mut_ptr().cast::<BranchPageElement>(),
                    count,
                )),
                Flags::LEAVES => PageData::Leaves(std::slice::from_raw_parts(
                    self.get_data_mut_ptr().cast::<LeafPageElement>(),
                    count,
                )),
                Flags::META => PageData::Meta(&*self.get_data_mut_ptr().cast::<Meta>()),
                Flags::FREELIST => {
                    let ptr = self.get_data_mut_ptr().cast::<PGID>();
                    let len = if count == 0xFFFF {
                        1 + *ptr as usize
                    } else {
                        count
                    };
                    PageData::Freelist(std::slice::from_raw_parts_mut(ptr, len))
                }
                _ => return Err(format!("Unknown page type: {}", self.flags).into()),
            })
        }
    }

    #[cfg_attr(not(test), expect(dead_code))]
    pub(crate) fn byte_size(&self) -> Result<usize, Error> {
        let mut size = Self::header_size();
        match self.data()? {
            PageData::Branches(b) => {
                if let Some(last) = b.last() {
                    size += (b.len() - 1) * BranchPageElement::SIZE;
                    size += (last.pos + last.ksize) as usize;
                }
            }
            PageData::Leaves(l) => {
                if let Some(last) = l.last() {
                    size += (l.len() - 1) * LeafPageElement::SIZE;
                    size += (last.pos + last.ksize + last.vsize) as usize;
                }
            }
            PageData::Meta(_) => {
                size += Meta::SIZE;
            }
            PageData::Freelist(f) => {
                size += size_of_val(f);
            }
        }
        Ok(size)
    }

    pub(crate) fn copy_data_from(&mut self, source: PageData) {
        let sbytes = source.as_bytes();
        match source {
            PageData::Freelist(c) => {
                self.count = c.len() as u16;
                self.flags = Flags::FREELIST;
            }
            PageData::Meta(_) => {
                self.count = 0;
                self.flags = Flags::META;
            }
            PageData::Leaves(c) => {
                self.count = c.len() as u16;
                self.flags = Flags::LEAVES;
            }
            PageData::Branches(c) => {
                self.count = c.len() as u16;
                self.flags = Flags::BRANCHES;
            }
        }
        unsafe {
            std::ptr::copy_nonoverlapping(sbytes.as_ptr(), self.get_data_mut_ptr(), sbytes.len())
        };
    }

    /// Retrieves a list of branch nodes
    pub(crate) fn branch_page_elements(&self) -> Result<&[BranchPageElement], Error> {
        match self.data()? {
            PageData::Branches(l) => Ok(l),
            _ => Err("Flags are not branches".into()),
        }
    }

    /// Retrieves a list of branch nodes
    #[expect(dead_code)]
    pub(crate) fn branch_page_elements_mut(&mut self) -> Result<&mut [BranchPageElement], Error> {
        unsafe {
            #[expect(invalid_reference_casting)]
            let elements = &mut *(self.branch_page_elements()? as *const [BranchPageElement]
                as *mut [BranchPageElement]);
            Ok(elements)
        }
    }

    /// Retrieves the branch node by index
    pub(crate) fn branch_page_element(&self, index: usize) -> Result<&BranchPageElement, Error> {
        self.branch_page_elements()?
            .get(index)
            .ok_or_else(|| format!("Page {}: element {index} out of range", self.id).into())
    }

    /// Retrieves the branch node by index
    pub(crate) fn branch_page_element_mut(
        &mut self,
        index: usize,
    ) -> Result<&mut BranchPageElement, Error> {
        unsafe {
            #[expect(invalid_reference_casting)]
            let element = &mut *(self.branch_page_element(index)? as *const BranchPageElement
                as *mut BranchPageElement);
            Ok(element)
        }
    }

    /// Retrieves a list of leaf nodes
    pub(crate) fn leaf_page_elements(&self) -> Result<&[LeafPageElement], Error> {
        match self.data()? {
            PageData::Leaves(l) => Ok(l),
            _ => Err("Flags are not leaves".into()),
        }
    }

    /// Retrieves a list of leaf nodes
    #[expect(dead_code)]
    pub(crate) fn leaf_page_elements_mut(&mut self) -> Result<&mut [LeafPageElement], Error> {
        unsafe {
            #[expect(invalid_reference_casting)]
            let elements = &mut *(self.leaf_page_elements()? as *const [LeafPageElement]
                as *mut [LeafPageElement]);
            Ok(elements)
        }
    }

    /// Retrieves the leaf node by index
    pub(crate) fn leaf_page_element(&self, index: usize) -> Result<&LeafPageElement, Error> {
        self.leaf_page_elements()?
            .get(index)
            .ok_or_else(|| format!("Page {}: element {index} out of range", self.id).into())
    }

    /// Retrieves the leaf node by index
    pub(crate) fn leaf_page_element_mut(
        &mut self,
        index: usize,
    ) -> Result<&mut LeafPageElement, Error> {
        unsafe {
            #[expect(invalid_reference_casting)]
            let element = &mut *(self.leaf_page_element(index)? as *const LeafPageElement
                as *mut LeafPageElement);
            Ok(element)
        }
    }

    /// returns a pointer to the metadata section of the page.
    pub(crate) fn meta(&self) -> Result<&Meta, Error> {
        match self.data()? {
            PageData::Meta(meta) => Ok(meta),
            _ => Err("Flags are not meta".into()),
        }
    }

    /// returns a pointer to the metadata section of the page.
    pub(crate) fn meta_mut(&mut self) -> Result<&mut Meta, Error> {
        unsafe {
            #[expect(invalid_reference_casting)]
            let meta = &mut *(self.meta()? as *const Meta as *mut Meta);
            Ok(meta)
        }
    }

    /// meta returns a pointer to the freelist section of the page.
    pub(crate) fn freelist(&self) -> Result<&[PGID], Error> {
        match self.data()? {
            PageData::Freelist(list) => Ok(list),
            _ => Err("Flags are not freelist".into()),
        }
    }

    /// meta returns a pointer to the freelist section of the page.
    pub(crate) fn freelist_mut(&mut self) -> Result<&mut [PGID], Error> {
        unsafe {
            #[expect(invalid_reference_casting)]
            let list = &mut *(self.freelist()? as *const [PGID] as *mut [PGID]);
            Ok(list)
        }
    }
}

impl Page {
    #[cfg_attr(not(test), expect(dead_code))]
    pub(crate) fn try_to_owned(&self) -> Result<OwnedPage, Error> {
        let ptr = (self as *const Page).cast::<u8>();
        unsafe {
            let slice = std::slice::from_raw_parts(ptr, self.byte_size()?).to_owned();
            Ok(OwnedPage::from_vec(slice))
        }
    }
}

#[derive(Clone)]
#[repr(align(64))]
pub(crate) struct OwnedPage {
    page: Vec<u8>,
}

// unsafe impl Send for Page {}
// unsafe impl Sync for Page {}

impl OwnedPage {
    pub(crate) fn new(size: PageSize) -> Self {
        Self {
            page: vec![0u8; size],
        }
    }

    pub(crate) fn from_vec(buf: Vec<u8>) -> Self {
        Self { page: buf }
    }

    /// returns page size
    #[inline]
    pub(crate) fn size(&self) -> PageSize {
        self.page.len()
    }

    /// reservec capacity of underlying vector to size
    #[expect(dead_code)]
    pub(crate) fn reserve(&mut self, size: usize) {
        self.page.reserve(size)
    }

    /// Returns pointer to page structure
    #[inline]
    pub(crate) fn as_ptr(&self) -> *const u8 {
        self.page.as_ptr()
    }

    /// Returns pointer to page structure
    #[expect(dead_code)]
    #[inline]
    pub(crate) fn as_mut_ptr(&mut self) -> *mut u8 {
        self.page.as_mut_ptr()
    }

    /// Returns binary serialized buffer of a page
    #[inline]
    pub(crate) fn buf(&self) -> &[u8] {
        &self.page
    }

    /// Returns binary serialized muttable buffer of a page
    #[inline]
    pub(crate) fn buf_mut(&mut self) -> &mut [u8] {
        &mut self.page
    }
}

impl Borrow<Page> for OwnedPage {
    #[inline]
    fn borrow(&self) -> &Page {
        unsafe { &*self.page.as_ptr().cast::<Page>() }
    }
}

impl BorrowMut<Page> for OwnedPage {
    #[inline]
    fn borrow_mut(&mut self) -> &mut Page {
        unsafe { &mut *self.page.as_mut_ptr().cast::<Page>() }
    }
}

impl Deref for OwnedPage {
    type Target = Page;

    #[inline]
    fn deref(&self) -> &Page {
        self.borrow()
    }
}

impl DerefMut for OwnedPage {
    #[inline]
    fn deref_mut(&mut self) -> &mut Page {
        self.borrow_mut()
    }
}

impl fmt::Debug for OwnedPage {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("OwnedPage")
            .field("size", &self.page.len())
            .field("page", self as &Page)
            .finish()
    }
}

pub(crate) fn merge_pgids(a: &[PGID], b: &[PGID]) -> Vec<PGID> {
    let mut dst = Vec::with_capacity(a.len() + b.len());
    dst.extend(a);
    dst.extend(b);
    dst.sort_unstable();
    dst
}
