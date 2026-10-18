use super::{OwnedPage, Page, PageData, leaf_page_element::LeafPageElement};
use crate::consts::{Flags, PGID};
use crate::errors::Error;
use crate::meta::Meta;

#[test]
fn new() {
    let mut buf = vec![0u8; 256];
    let page = Page::from_buf_mut(&mut buf);
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
    page.copy_data_from(PageData::Freelist(ids));

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
    let nodes = unsafe {
        std::slice::from_raw_parts_mut(page.get_data_mut_ptr().cast::<LeafPageElement>(), 3)
    };
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

    let data = unsafe { std::slice::from_raw_parts_mut((&raw mut nodes[2]).cast::<u8>(), 4096) };
    data[..7].copy_from_slice("barfooz".as_bytes());
    data[7..7 + 13].copy_from_slice("helloworldbye".as_bytes());

    let el = page.leaf_page_element(0).unwrap();
    assert_eq!(el.ksize, 3);
    assert_eq!(el.vsize, 4);
    assert_eq!(el.pos, 32);

    let el = page.leaf_page_element(1).unwrap();
    assert_eq!(el.ksize, 10);
    assert_eq!(el.vsize, 3);
    assert_eq!(el.pos, 23);
}

#[test]
fn to_owned() {
    let mut buf = vec![0u8; 1024];
    let page = Page::from_buf_mut(&mut buf);
    page.flags = Flags::LEAVES;
    page.count = 2;
    let nodes = unsafe {
        std::slice::from_raw_parts_mut(page.get_data_mut_ptr().cast::<LeafPageElement>(), 3)
    };
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

    let data = unsafe { std::slice::from_raw_parts_mut((&raw mut nodes[2]).cast::<u8>(), 4096) };
    data[..7].copy_from_slice("barfooz".as_bytes());
    data[7..7 + 13].copy_from_slice("helloworldbye".as_bytes());

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
fn check_bounds() {
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
    assert!(page.check_bounds(4096).is_ok());
    assert_eq!(page.check_bounds(8).err(), out_of_bounds);

    page.leaf_page_element_mut(0).unwrap().vsize = 5000;
    assert_eq!(page.check_bounds(4096).err(), out_of_bounds);

    page.count = 1000;
    assert_eq!(page.check_bounds(4096).err(), out_of_bounds);

    page.flags = Flags::META;
    assert!(page.check_bounds(Page::header_size() + Meta::SIZE).is_ok());
    assert_eq!(
        page.check_bounds(Page::header_size() + Meta::SIZE - 1)
            .err(),
        out_of_bounds
    );

    page.flags = Flags::FREELIST;
    page.count = 0xFFFF;
    unsafe { *page.get_data_mut_ptr().cast::<PGID>() = 10_000 };
    assert_eq!(page.check_bounds(4096).err(), out_of_bounds);
    unsafe { *page.get_data_mut_ptr().cast::<PGID>() = u64::MAX };
    assert_eq!(page.check_bounds(4096).err(), out_of_bounds);
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
