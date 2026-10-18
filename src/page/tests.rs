use super::{OwnedPage, Page, PageData, leaf_page_element::LeafPageElement};
use crate::consts::{Flags, PGID};
use crate::errors::Error;
use crate::meta::Meta;

#[test]
fn new() {
    let mut buf = OwnedPage::new(256);
    let page = Page::from_buf_mut(buf.buf_mut()).unwrap();
    assert_eq!(page.overflow, 0);
    assert_eq!(page.count, 0);
    assert_eq!(page.id, 0);

    page.id = 25;
    assert_eq!(page.id, 25);

    page.flags = Flags::META;
    assert_eq!(page.flags, Flags::META);
}

#[test]
fn copy_data_from() {
    let mut page = OwnedPage::new(1024);
    page.flags = Flags::FREELIST;
    let ids: &[PGID] = &[23, 50];
    page.copy_data_from(PageData::Freelist(ids)).unwrap();

    assert_eq!(page.count, 2);
    match page.data().unwrap() {
        PageData::Freelist(l) => {
            assert_eq!(&[23, 50], l);
        }
        _ => panic!("wrong page type"),
    }
}

#[test]
fn read_leaf_nodes() {
    let mut page = OwnedPage::new(4096);
    page.flags = Flags::LEAVES;
    page.count = 2;
    {
        let nodes = page.leaf_page_elements_mut().unwrap();
        nodes[0] = LeafPageElement {
            flags: 0,
            pos: 32,
            ksize: 3,
            vsize: 4,
        };
        nodes[1] = LeafPageElement {
            flags: 0,
            pos: 23,
            ksize: 10,
            vsize: 3,
        };
    }
    page.slice_mut(32, 7).unwrap().copy_from_slice(b"barfooz");
    page.slice_mut(39, 13)
        .unwrap()
        .copy_from_slice(b"helloworldbye");

    let el = page.leaf_page_element(0).unwrap();
    assert_eq!(el.ksize, 3);
    assert_eq!(el.vsize, 4);
    assert_eq!(el.pos, 32);

    let el = page.leaf_page_element(1).unwrap();
    assert_eq!(el.ksize, 10);
    assert_eq!(el.vsize, 3);
    assert_eq!(el.pos, 23);

    assert_eq!(page.leaf_key(0).unwrap(), b"bar");
    assert_eq!(page.leaf_value(0).unwrap(), b"fooz");
    assert_eq!(page.leaf_key(1).unwrap(), b"helloworld");
    assert_eq!(page.leaf_value(1).unwrap(), b"bye");
}

#[test]
fn to_owned() {
    let mut page = OwnedPage::new(1024);
    page.flags = Flags::LEAVES;
    page.count = 2;
    {
        let nodes = page.leaf_page_elements_mut().unwrap();
        nodes[0] = LeafPageElement {
            flags: 0,
            pos: 32,
            ksize: 3,
            vsize: 4,
        };
        nodes[1] = LeafPageElement {
            flags: 0,
            pos: 23,
            ksize: 10,
            vsize: 3,
        };
    }
    page.slice_mut(32, 7).unwrap().copy_from_slice(b"barfooz");
    page.slice_mut(39, 13)
        .unwrap()
        .copy_from_slice(b"helloworldbye");

    let owned = page.try_to_owned().unwrap();
    assert_eq!(
        owned.size(),
        Page::header_size() + LeafPageElement::SIZE + 23 + 10 + 3
    );
}

#[test]
fn accessors_return_errors() {
    let mut page = OwnedPage::new(1024);
    page.id = 7;
    page.flags = Flags::from_bits_retain(0x20);
    assert!(page.data().is_err());

    page.flags = Flags::BRANCHES;
    page.count = 1;
    assert!(page.leaf_page_elements().is_err());
    assert!(page.branch_page_element(0).is_ok());
    assert_eq!(
        page.branch_page_element(1).err(),
        Some(Error::CheckFail(vec![
            "Page 7: element 1 out of range".to_string()
        ]))
    );
}

#[test]
fn accessors_check_bounds() {
    let out_of_bounds = Some(Error::CheckFail(vec![
        "Page 3: data out of bounds".to_string(),
    ]));
    let mut page = OwnedPage::new(4096);
    page.id = 3;
    page.flags = Flags::LEAVES;
    page.count = 1;
    {
        let elem = page.leaf_page_element_mut(0).unwrap();
        elem.pos = 16;
        elem.ksize = 3;
        elem.vsize = 4;
    }
    assert!(page.leaf_key(0).is_ok());
    assert!(page.leaf_value(0).is_ok());

    page.leaf_page_element_mut(0).unwrap().vsize = 5000;
    assert!(page.leaf_key(0).is_ok());
    assert_eq!(page.leaf_value(0).err(), out_of_bounds);

    page.count = 1000;
    assert_eq!(page.leaf_page_elements().err(), out_of_bounds);

    page.flags = Flags::FREELIST;
    page.count = 0xFFFF;
    page.slice_mut(0, 8)
        .unwrap()
        .copy_from_slice(&10_000u64.to_ne_bytes());
    assert_eq!(page.freelist().err(), out_of_bounds);
    page.slice_mut(0, 8)
        .unwrap()
        .copy_from_slice(&u64::MAX.to_ne_bytes());
    assert_eq!(page.freelist().err(), out_of_bounds);

    let mut meta = OwnedPage::new(Page::header_size() + Meta::SIZE - 1);
    meta.flags = Flags::META;
    assert!(meta.meta().is_err());
    let mut meta = OwnedPage::new(Page::header_size() + Meta::SIZE);
    meta.flags = Flags::META;
    assert!(meta.meta().is_ok());

    assert!(Page::from_buf(&OwnedPage::new(8).buf()[..8]).is_err());
}

#[test]
fn header_size_matches_layout() {
    let page = OwnedPage::new(64);
    let page: &Page = &page;
    let offset = unsafe {
        page.data
            .as_ptr()
            .offset_from((page as *const Page).cast::<u8>())
    };
    assert_eq!(offset as usize, Page::header_size());
    assert_eq!(Page::header_size(), 16);
}

#[test]
fn owned_page_is_aligned() {
    for size in [5, 16, 1000, 4096] {
        let page = OwnedPage::new(size);
        assert!(page.as_ptr().cast::<u64>().is_aligned());
        assert_eq!(page.buf().len(), size);
    }
    let page = OwnedPage::from_vec(vec![1, 2, 3]);
    assert!(page.as_ptr().cast::<u64>().is_aligned());
    assert_eq!(page.buf(), &[1, 2, 3]);
}
