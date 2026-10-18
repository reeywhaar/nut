use super::FreeList;
use crate::consts::{Flags, PGID};
use crate::errors::Error;
use crate::page::{OwnedPage, PageData};

#[test]
fn free() {
    let mut f = FreeList::default();
    let page = {
        let mut page = OwnedPage::new(1024);
        page.id = 12;
        page.overflow = 0;
        page.flags = Flags::FREELIST;
        page
    };
    f.free(100, &page).unwrap();
    assert_eq!(&vec![12], f.pending.get(&100).unwrap());
}

#[test]
fn free_overflow() {
    let mut f = FreeList::default();
    let page = {
        let mut page = OwnedPage::new(1024);
        page.id = 12;
        page.overflow = 3;
        page.flags = Flags::FREELIST;
        page
    };
    f.free(100, &page).unwrap();
    assert_eq!(&vec![12, 13, 14, 15], f.pending.get(&100).unwrap());
}

#[test]
fn release() {
    let mut f = FreeList::default();
    let page = {
        let mut page = OwnedPage::new(1024);
        page.id = 12;
        page.overflow = 1;
        page.flags = Flags::FREELIST;
        page
    };
    f.free(100, &page).unwrap();
    let page = {
        let mut page = OwnedPage::new(1024);
        page.id = 9;
        page.overflow = 0;
        page.flags = Flags::FREELIST;
        page
    };
    f.free(100, &page).unwrap();
    let page = {
        let mut page = OwnedPage::new(1024);
        page.id = 39;
        page.overflow = 0;
        page.flags = Flags::FREELIST;
        page
    };
    f.free(102, &page).unwrap();
    f.release(100);
    f.release(101);

    assert_eq!(vec![9, 12, 13], f.ids);

    f.release(102);
    assert_eq!(vec![9, 12, 13, 39], f.ids);
}

#[test]
fn allocate() {
    let mut f = FreeList {
        ids: vec![3, 4, 5, 6, 7, 9, 12, 13, 18],
        ..Default::default()
    };

    assert_eq!(Ok(3), f.allocate(3));
    assert_eq!(vec![6, 7, 9, 12, 13, 18], f.ids);
    assert_eq!(Ok(6), f.allocate(1));
    assert_eq!(vec![7, 9, 12, 13, 18], f.ids);
    assert_eq!(Ok(0), f.allocate(3));
    assert_eq!(vec![7, 9, 12, 13, 18], f.ids);
    assert_eq!(Ok(12), f.allocate(2));
    assert_eq!(vec![7, 9, 18], f.ids);
    assert_eq!(Ok(7), f.allocate(1));
    assert_eq!(vec![9, 18], f.ids);
    assert_eq!(Ok(0), f.allocate(0));
    assert_eq!(vec![9, 18], f.ids);
    assert_eq!(Ok(0), f.allocate(0));
    assert_eq!(vec![9, 18], f.ids);
    assert_eq!(Ok(9), f.allocate(1));
    assert_eq!(vec![18], f.ids);
    assert_eq!(Ok(18), f.allocate(1));
    assert_eq!(Vec::<PGID>::new(), f.ids);
    assert_eq!(Ok(0), f.allocate(1));
    assert_eq!(Vec::<PGID>::new(), f.ids);
}

#[test]
fn read() {
    let mut page = {
        let mut page = OwnedPage::new(1024);
        page.id = 1;
        page.overflow = 0;
        page.flags = Flags::FREELIST;
        page
    };
    let ids: &[PGID] = &[23, 50];
    page.copy_data_from(PageData::Freelist(ids));

    // Deserialize page into a freelist.
    let mut f = FreeList::default();
    f.read(&page).unwrap();

    assert_eq!(&[23 as PGID, 50], f.ids.as_slice());
}

#[test]
fn write() {
    let mut fl = FreeList {
        ids: vec![12, 39],
        ..Default::default()
    };
    fl.pending.insert(100, vec![28, 11]);
    fl.pending.insert(101, vec![3]);

    let mut page = {
        let mut page = OwnedPage::new(1024);
        page.id = 1;
        page.overflow = 0;
        page.flags = Flags::FREELIST;
        page
    };

    fl.write(&mut page).unwrap();

    let mut fl2 = FreeList::default();
    fl2.read(&page).unwrap();

    assert_eq!(vec![3, 11, 12, 28, 39], fl2.ids);
}

#[test]
fn allocate_meta_page() {
    let mut f = FreeList {
        ids: vec![1, 2, 3],
        ..Default::default()
    };
    assert_eq!(
        f.allocate(1),
        Err(Error::Unexpected("Invalid page allocation: 1".to_string()))
    );
}

#[test]
fn read_wrong_page_type() {
    let mut page = OwnedPage::new(1024);
    page.flags = Flags::LEAVES;
    let mut f = FreeList::default();
    assert_eq!(
        f.read(&page),
        Err(Error::Unexpected("Freelist flags incorrect: 2".to_string()))
    );
}

#[test]
fn rollback_missing_cache_entry() {
    let mut f = FreeList::default();
    f.pending.insert(1, vec![5]);
    assert_eq!(
        f.rollback(1),
        Err(Error::Unexpected(
            "Pending page 5 missing from cache".to_string()
        ))
    );
}

#[test]
fn write_read_overflow() {
    let ids: Vec<PGID> = (2..70_002).collect();
    let mut fl = FreeList {
        ids: ids.clone(),
        ..Default::default()
    };
    let mut page = OwnedPage::new(fl.size());
    fl.write(&mut page).unwrap();
    assert_eq!(page.count, 0xFFFF);
    assert_eq!(page.freelist().unwrap()[..2], [70_000, 2]);

    let mut fl2 = FreeList::default();
    fl2.read(&page).unwrap();
    assert_eq!(fl2.ids, ids);
}
