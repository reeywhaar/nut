#[cfg(test)]
pub mod page_data_tests;
#[cfg(test)]
pub mod tests;

use std::borrow::{Borrow, BorrowMut};
use std::fmt;
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
    /// page data following the header
    data: [u8],
}

impl fmt::Debug for Page {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Page")
            .field("id", &self.id)
            .field("flags", &self.flags)
            .field("count", &self.count)
            .field("overflow", &self.overflow)
            .field("data_len", &self.data.len())
            .finish()
    }
}

impl Page {
    pub(crate) const fn header_size() -> usize {
        size_of::<PGID>() + size_of::<Flags>() + size_of::<u16>() + size_of::<PageSpan>()
    }

    pub(crate) fn from_buf(buf: &[u8]) -> Result<&Page, Error> {
        let len = buf
            .len()
            .checked_sub(Self::header_size())
            .ok_or_else(|| Error::corrupted("Page buffer too small"))?;
        debug_assert!(buf.as_ptr().cast::<u64>().is_aligned());
        Ok(unsafe { &*(std::ptr::slice_from_raw_parts(buf.as_ptr(), len) as *const Page) })
    }

    pub(crate) fn from_buf_mut(buf: &mut [u8]) -> Result<&mut Page, Error> {
        let len = buf
            .len()
            .checked_sub(Self::header_size())
            .ok_or_else(|| Error::corrupted("Page buffer too small"))?;
        debug_assert!(buf.as_ptr().cast::<u64>().is_aligned());
        Ok(unsafe {
            &mut *(std::ptr::slice_from_raw_parts_mut(buf.as_mut_ptr(), len) as *mut Page)
        })
    }

    fn out_of_bounds(&self) -> Error {
        Error::corrupted(format!("Page {}: data out of bounds", self.id))
    }

    #[inline]
    fn elements<T>(&self, count: usize) -> Result<&[T], Error> {
        if count
            .checked_mul(size_of::<T>())
            .is_none_or(|size| size > self.data.len())
        {
            return Err(self.out_of_bounds());
        }
        Ok(unsafe { std::slice::from_raw_parts(self.data.as_ptr().cast::<T>(), count) })
    }

    fn elements_mut<T>(&mut self, count: usize) -> Result<&mut [T], Error> {
        if count
            .checked_mul(size_of::<T>())
            .is_none_or(|size| size > self.data.len())
        {
            return Err(self.out_of_bounds());
        }
        Ok(unsafe { std::slice::from_raw_parts_mut(self.data.as_mut_ptr().cast::<T>(), count) })
    }

    #[inline]
    fn element_data(&self, elem_offset: usize, pos: u32, len: u32) -> Result<&[u8], Error> {
        elem_offset
            .checked_add(pos as usize)
            .and_then(|start| Some(start..start.checked_add(len as usize)?))
            .and_then(|range| self.data.get(range))
            .ok_or_else(|| self.out_of_bounds())
    }

    pub(crate) fn slice_mut(&mut self, start: usize, len: usize) -> Result<&mut [u8], Error> {
        let id = self.id;
        start
            .checked_add(len)
            .and_then(|end| self.data.get_mut(start..end))
            .ok_or_else(|| Error::corrupted(format!("Page {id}: data out of bounds")))
    }

    fn freelist_len(&self) -> Result<usize, Error> {
        let count = self.count as usize;
        if count != 0xFFFF {
            return Ok(count);
        }
        let first = self.elements::<PGID>(1)?[0];
        usize::try_from(first)
            .ok()
            .and_then(|len| len.checked_add(1))
            .ok_or_else(|| self.out_of_bounds())
    }

    pub(crate) fn data(&self) -> Result<PageData<'_>, Error> {
        let count = self.count as usize;
        Ok(match self.flags {
            Flags::BRANCHES => PageData::Branches(self.elements(count)?),
            Flags::LEAVES => PageData::Leaves(self.elements(count)?),
            Flags::META => PageData::Meta(&self.elements::<Meta>(1)?[0]),
            Flags::FREELIST => PageData::Freelist(self.elements(self.freelist_len()?)?),
            _ => {
                return Err(Error::corrupted(format!(
                    "Unknown page type: {}",
                    self.flags
                )));
            }
        })
    }

    #[cfg_attr(not(test), expect(dead_code))]
    pub(crate) fn byte_size(&self) -> Result<usize, Error> {
        let mut size = Self::header_size();
        match self.data()? {
            PageData::Branches(b) => {
                if let Some(last) = b.last() {
                    size += (b.len() - 1) * BranchPageElement::SIZE;
                    size += last.pos as usize + last.ksize as usize;
                }
            }
            PageData::Leaves(l) => {
                if let Some(last) = l.last() {
                    size += (l.len() - 1) * LeafPageElement::SIZE;
                    size += last.pos as usize + last.ksize as usize + last.vsize as usize;
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

    pub(crate) fn copy_data_from(&mut self, source: PageData) -> Result<(), Error> {
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
        self.slice_mut(0, sbytes.len())?.copy_from_slice(sbytes);
        Ok(())
    }

    /// Retrieves a list of branch nodes
    #[inline]
    pub(crate) fn branch_page_elements(&self) -> Result<&[BranchPageElement], Error> {
        if self.flags != Flags::BRANCHES {
            return Err(Error::corrupted("Flags are not branches"));
        }
        self.elements(self.count as usize)
    }

    /// Retrieves a list of branch nodes
    pub(crate) fn branch_page_elements_mut(&mut self) -> Result<&mut [BranchPageElement], Error> {
        if self.flags != Flags::BRANCHES {
            return Err(Error::corrupted("Flags are not branches"));
        }
        self.elements_mut(self.count as usize)
    }

    /// Retrieves the branch node by index
    #[inline]
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

    /// Retrieves the key of the branch node by index
    pub(crate) fn branch_key(&self, index: usize) -> Result<&[u8], Error> {
        self.branch_key_of(index, self.branch_page_element(index)?)
    }

    #[inline]
    pub(crate) fn branch_key_of(
        &self,
        index: usize,
        elem: &BranchPageElement,
    ) -> Result<&[u8], Error> {
        self.element_data(index * BranchPageElement::SIZE, elem.pos, elem.ksize)
    }

    /// Retrieves a list of leaf nodes
    #[inline]
    pub(crate) fn leaf_page_elements(&self) -> Result<&[LeafPageElement], Error> {
        if self.flags != Flags::LEAVES {
            return Err(Error::corrupted("Flags are not leaves"));
        }
        self.elements(self.count as usize)
    }

    /// Retrieves a list of leaf nodes
    pub(crate) fn leaf_page_elements_mut(&mut self) -> Result<&mut [LeafPageElement], Error> {
        if self.flags != Flags::LEAVES {
            return Err(Error::corrupted("Flags are not leaves"));
        }
        self.elements_mut(self.count as usize)
    }

    /// Retrieves the leaf node by index
    #[inline]
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

    /// Retrieves the key of the leaf node by index
    pub(crate) fn leaf_key(&self, index: usize) -> Result<&[u8], Error> {
        self.leaf_key_of(index, self.leaf_page_element(index)?)
    }

    #[inline]
    pub(crate) fn leaf_key_of(&self, index: usize, elem: &LeafPageElement) -> Result<&[u8], Error> {
        self.element_data(index * LeafPageElement::SIZE, elem.pos, elem.ksize)
    }

    /// Retrieves the value of the leaf node by index
    pub(crate) fn leaf_value(&self, index: usize) -> Result<&[u8], Error> {
        self.leaf_value_of(index, self.leaf_page_element(index)?)
    }

    #[inline]
    pub(crate) fn leaf_value_of(
        &self,
        index: usize,
        elem: &LeafPageElement,
    ) -> Result<&[u8], Error> {
        let pos = elem
            .pos
            .checked_add(elem.ksize)
            .ok_or_else(|| self.out_of_bounds())?;
        self.element_data(index * LeafPageElement::SIZE, pos, elem.vsize)
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
        Ok(&mut self.elements_mut::<Meta>(1)?[0])
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
        let len = self.freelist_len()?;
        self.elements_mut(len)
    }

    #[cfg_attr(not(test), expect(dead_code))]
    pub(crate) fn try_to_owned(&self) -> Result<OwnedPage, Error> {
        let size = self.byte_size()?;
        if size > Self::header_size() + self.data.len() {
            return Err(self.out_of_bounds());
        }
        let bytes = unsafe { std::slice::from_raw_parts((self as *const Page).cast::<u8>(), size) };
        Ok(OwnedPage::from_vec(bytes.to_vec()))
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
        let len = self.len.saturating_sub(Page::header_size());
        unsafe { &*(std::ptr::slice_from_raw_parts(self.as_ptr(), len) as *const Page) }
    }
}

impl BorrowMut<Page> for OwnedPage {
    #[inline]
    fn borrow_mut(&mut self) -> &mut Page {
        let len = self.len.saturating_sub(Page::header_size());
        unsafe { &mut *(std::ptr::slice_from_raw_parts_mut(self.as_mut_ptr(), len) as *mut Page) }
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
            .field("page", &&**self)
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
