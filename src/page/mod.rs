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

    pub(crate) fn check_bounds(&self, len: usize) -> Result<(), Error> {
        let out_of_bounds = || Error::corrupted(format!("Page {}: data out of bounds", self.id));
        let header = Self::header_size() as u64;
        let len = len as u64;
        let count = u64::from(self.count);
        if header > len {
            return Err(out_of_bounds());
        }
        match self.flags {
            Flags::LEAVES => {
                if header + count * LeafPageElement::SIZE as u64 > len {
                    return Err(out_of_bounds());
                }
                for (i, elem) in self.leaf_page_elements()?.iter().enumerate() {
                    let offset = header + i as u64 * LeafPageElement::SIZE as u64;
                    let end = offset
                        + u64::from(elem.pos)
                        + u64::from(elem.ksize)
                        + u64::from(elem.vsize);
                    if end > len {
                        return Err(out_of_bounds());
                    }
                }
            }
            Flags::BRANCHES => {
                if header + count * BranchPageElement::SIZE as u64 > len {
                    return Err(out_of_bounds());
                }
                for (i, elem) in self.branch_page_elements()?.iter().enumerate() {
                    let offset = header + i as u64 * BranchPageElement::SIZE as u64;
                    if offset + u64::from(elem.pos) + u64::from(elem.ksize) > len {
                        return Err(out_of_bounds());
                    }
                }
            }
            Flags::META => {
                if header + Meta::SIZE as u64 > len {
                    return Err(out_of_bounds());
                }
            }
            Flags::FREELIST => {
                let mut ids = count;
                if count == 0xFFFF {
                    if header + size_of::<PGID>() as u64 > len {
                        return Err(out_of_bounds());
                    }
                    ids = unsafe { *self.get_data_ptr().cast::<PGID>() }
                        .checked_add(1)
                        .ok_or_else(out_of_bounds)?;
                }
                let end = ids
                    .checked_mul(size_of::<PGID>() as u64)
                    .and_then(|size| size.checked_add(header));
                if end.is_none_or(|end| end > len) {
                    return Err(out_of_bounds());
                }
            }
            _ => {}
        }
        Ok(())
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
                _ => {
                    return Err(Error::corrupted(format!(
                        "Unknown page type: {}",
                        self.flags
                    )));
                }
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
                _ => {
                    return Err(Error::corrupted(format!(
                        "Unknown page type: {}",
                        self.flags
                    )));
                }
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
            _ => Err(Error::corrupted("Flags are not branches")),
        }
    }

    /// Retrieves a list of branch nodes
    pub(crate) fn branch_page_elements_mut(&mut self) -> Result<&mut [BranchPageElement], Error> {
        if self.flags != Flags::BRANCHES {
            return Err(Error::corrupted("Flags are not branches"));
        }
        let count = self.count as usize;
        Ok(unsafe { std::slice::from_raw_parts_mut(self.get_data_mut_ptr().cast(), count) })
    }

    /// Retrieves the branch node by index
    pub(crate) fn branch_page_element(&self, index: usize) -> Result<&BranchPageElement, Error> {
        self.branch_page_elements()?.get(index).ok_or_else(|| {
            Error::corrupted(format!("Page {}: element {index} out of range", self.id))
        })
    }

    /// Retrieves the branch node by index
    pub(crate) fn branch_page_element_mut(
        &mut self,
        index: usize,
    ) -> Result<&mut BranchPageElement, Error> {
        let id = self.id;
        self.branch_page_elements_mut()?
            .get_mut(index)
            .ok_or_else(|| Error::corrupted(format!("Page {id}: element {index} out of range")))
    }

    /// Retrieves a list of leaf nodes
    pub(crate) fn leaf_page_elements(&self) -> Result<&[LeafPageElement], Error> {
        match self.data()? {
            PageData::Leaves(l) => Ok(l),
            _ => Err(Error::corrupted("Flags are not leaves")),
        }
    }

    /// Retrieves a list of leaf nodes
    pub(crate) fn leaf_page_elements_mut(&mut self) -> Result<&mut [LeafPageElement], Error> {
        if self.flags != Flags::LEAVES {
            return Err(Error::corrupted("Flags are not leaves"));
        }
        let count = self.count as usize;
        Ok(unsafe { std::slice::from_raw_parts_mut(self.get_data_mut_ptr().cast(), count) })
    }

    /// Retrieves the leaf node by index
    pub(crate) fn leaf_page_element(&self, index: usize) -> Result<&LeafPageElement, Error> {
        self.leaf_page_elements()?.get(index).ok_or_else(|| {
            Error::corrupted(format!("Page {}: element {index} out of range", self.id))
        })
    }

    /// Retrieves the leaf node by index
    pub(crate) fn leaf_page_element_mut(
        &mut self,
        index: usize,
    ) -> Result<&mut LeafPageElement, Error> {
        let id = self.id;
        self.leaf_page_elements_mut()?
            .get_mut(index)
            .ok_or_else(|| Error::corrupted(format!("Page {id}: element {index} out of range")))
    }

    /// returns a pointer to the metadata section of the page.
    pub(crate) fn meta(&self) -> Result<&Meta, Error> {
        match self.data()? {
            PageData::Meta(meta) => Ok(meta),
            _ => Err(Error::corrupted("Flags are not meta")),
        }
    }

    /// returns a pointer to the metadata section of the page.
    pub(crate) fn meta_mut(&mut self) -> Result<&mut Meta, Error> {
        if self.flags != Flags::META {
            return Err(Error::corrupted("Flags are not meta"));
        }
        Ok(unsafe { &mut *self.get_data_mut_ptr().cast::<Meta>() })
    }

    /// meta returns a pointer to the freelist section of the page.
    pub(crate) fn freelist(&self) -> Result<&[PGID], Error> {
        match self.data()? {
            PageData::Freelist(list) => Ok(list),
            _ => Err(Error::corrupted("Flags are not freelist")),
        }
    }

    /// meta returns a pointer to the freelist section of the page.
    pub(crate) fn freelist_mut(&mut self) -> Result<&mut [PGID], Error> {
        if self.flags != Flags::FREELIST {
            return Err(Error::corrupted("Flags are not freelist"));
        }
        let count = self.count as usize;
        let ptr = self.get_data_mut_ptr().cast::<PGID>();
        let len = if count == 0xFFFF {
            1 + unsafe { *ptr } as usize
        } else {
            count
        };
        Ok(unsafe { std::slice::from_raw_parts_mut(ptr, len) })
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
    page: Vec<u64>,
    len: usize,
}

// unsafe impl Send for Page {}
// unsafe impl Sync for Page {}

impl OwnedPage {
    pub(crate) fn new(size: PageSize) -> Self {
        let words = size.max(Page::header_size()).div_ceil(size_of::<u64>());
        Self {
            page: vec![0; words],
            len: size,
        }
    }

    pub(crate) fn from_vec(buf: Vec<u8>) -> Self {
        let mut page = Self::new(buf.len());
        page.buf_mut().copy_from_slice(&buf);
        page
    }

    /// returns page size
    #[inline]
    pub(crate) fn size(&self) -> PageSize {
        self.len
    }

    /// reservec capacity of underlying vector to size
    #[expect(dead_code)]
    pub(crate) fn reserve(&mut self, size: usize) {
        self.page.reserve(size.div_ceil(size_of::<u64>()))
    }

    /// Returns pointer to page structure
    #[inline]
    pub(crate) fn as_ptr(&self) -> *const u8 {
        self.page.as_ptr().cast()
    }

    /// Returns pointer to page structure
    #[inline]
    pub(crate) fn as_mut_ptr(&mut self) -> *mut u8 {
        self.page.as_mut_ptr().cast()
    }

    /// Returns binary serialized buffer of a page
    #[inline]
    pub(crate) fn buf(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.as_ptr(), self.len) }
    }

    /// Returns binary serialized muttable buffer of a page
    #[inline]
    pub(crate) fn buf_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.as_mut_ptr(), self.len) }
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
            .field("size", &self.len)
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
