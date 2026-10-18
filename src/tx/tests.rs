use super::{Tx, TxBuilder};
use crate::db::tests::db_mock;
use crate::db::{CheckMode, DBBuilder};
use crate::errors::Error;
use fnv::FnvHasher;
use std::hash::Hasher;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;

pub(crate) fn tx_mock() -> Tx {
    let tx = TxBuilder::new().writable(true).build().unwrap();
    tx.0.meta.write().pgid = 1;
    tx
}

#[test]
fn commit_empty() {
    let mut db = db_mock().build().unwrap();
    assert!(db.0.rw_tx.try_read().unwrap().is_none());

    {
        let tx = db.begin_rw_tx().unwrap();
        assert_eq!(Arc::strong_count(&tx.0), 2);
        assert!(tx.db().unwrap().0.rw_tx.try_read().unwrap().is_some());
    }
    assert!(db.0.rw_tx.try_read().unwrap().is_none());

    db.view(Box::new(|tx| {
        assert_eq!(tx.db().unwrap().0.txs.try_read().unwrap().len(), 1);
        Ok(())
    }))
    .unwrap();
    assert_eq!(db.0.txs.try_read().unwrap().len(), 0);
}

#[test]
fn commit_some() {
    let mut db = db_mock().build().unwrap();
    let mut tx = db.begin_rw_tx().unwrap();
    {
        let mut bucket = tx.create_bucket(b"bucket").unwrap();
        bucket.put(b"key", b"value".to_vec()).unwrap();
    }
    tx.commit().unwrap();
}

#[test]
fn commit_multiple() {
    let n_commits = 5;
    let n_values = 1000;
    let mut db = db_mock().build().unwrap();
    for i in 0..n_commits {
        let mut tx = db.begin_rw_tx().unwrap();
        let mut bucket = tx.create_bucket_if_not_exists(b"bucket").unwrap();
        for n in 0..n_values {
            bucket
                .put(format!("key-{i}-{n}").as_bytes(), b"value".to_vec())
                .unwrap();
        }
    }
}

#[test]
fn db_gone() {
    // handled statically!
    // let mut tx = {
    // 	let mut db = db_mock().build().unwrap();
    // 	db.begin_rw_tx().unwrap()
    // };
    // assert_eq!(tx.commit(), Err(Error::DatabaseGone));
}

#[test]
fn db_closed() {
    // handled statically!
    // let mut db = db_mock().build().unwrap();
    // let mut tx = db.begin_rw_tx().unwrap();
    // db.close().unwrap();
    // assert_eq!(tx.commit(), Err(Error::DatabaseGone));
}

#[test]
fn delete_bucket() {
    let mut db = db_mock().build().unwrap();
    let mut tx = db.begin_rw_tx().unwrap();
    {
        drop(tx.create_bucket(b"bucket").unwrap());
        drop(tx.bucket(b"bucket").unwrap());
        tx.delete_bucket(b"bucket").unwrap();
        tx.bucket(b"bucket").err().unwrap();
    }
    tx.commit().unwrap();
}

#[test]
///ensure that writes produce idempotent file
fn commit_hash_ensure() {
    for _ in 0..20 {
        let mut db = db_mock().page_size(4096).autoremove(true).build().unwrap();
        let mut tx = db.begin_rw_tx().unwrap();
        {
            let mut bucket = tx.create_bucket(b"bucket").unwrap();
            bucket.put(b"donald", b"trump".to_vec()).unwrap();
            bucket.put(b"barack", b"obama".to_vec()).unwrap();
            bucket.put(b"thomas", b"jefferson".to_vec()).unwrap();
        }
        tx.commit().unwrap();

        let mut file = db.0.file.try_write().unwrap();
        file.flush().unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();

        let mut hasher = FnvHasher::default();
        let mut buf = vec![0u8; db.page_size()];
        while let Ok(()) = file.get_ref().read_exact(&mut buf) {
            hasher.write(&buf);
        }
        let hash = hasher.finish();
        assert_eq!(hash, 9680149046811131486)
    }
}

#[test]
/// ensures that data actually written to disk
fn commit_ensure() {
    let path = {
        let mut db = db_mock().autoremove(false).build().unwrap();
        let mut tx = db.begin_rw_tx().unwrap();
        {
            let mut bucket = tx.create_bucket(b"bucket").unwrap();
            bucket.put(b"donald", b"trump".to_vec()).unwrap();
            bucket.put(b"barack", b"obama".to_vec()).unwrap();
            bucket.put(b"thomas", b"jefferson".to_vec()).unwrap();
        }
        tx.commit().unwrap();
        db.path().to_owned()
    };

    let db = DBBuilder::new(path)
        .autoremove(true)
        .read_only(false)
        .build()
        .unwrap();
    db.view(Box::new(|tx| -> Result<(), String> {
        let bucket = tx.bucket(b"bucket").unwrap();
        assert_eq!(
            bucket.try_get(b"donald").unwrap().expect("no donald"),
            b"trump"
        );
        assert_eq!(
            bucket.try_get(b"barack").unwrap().expect("no barack"),
            b"obama"
        );
        assert_eq!(
            bucket.try_get(b"thomas").unwrap().expect("no thomas"),
            b"jefferson"
        );
        Ok(())
    }))
    .unwrap();
}

#[test]
fn rollback_empty() {
    let db = db_mock().build().unwrap();
    assert!(db.0.rw_tx.try_read().unwrap().is_none());

    let tx = db.begin_tx().unwrap();
    tx.rollback().unwrap();
}

#[test]
fn rollback_some() {
    let mut db = db_mock().build().unwrap();
    let mut tx = db.begin_rw_tx().unwrap();
    {
        let mut bucket = tx.create_bucket(b"bucket").unwrap();
        bucket.put(b"key", b"value".to_vec()).unwrap();
    }
    tx.rollback().unwrap();
}

#[test]
fn for_each() {
    let mut db = db_mock().build().unwrap();
    let mut tx = db.begin_rw_tx().unwrap();
    {
        let mut bucket = tx.create_bucket(b"bucket").unwrap();
        bucket.put(b"key", b"value".to_vec()).unwrap();
        bucket.put(b"keys", b"value".to_vec()).unwrap();
    }
    {
        let mut bucket = tx.create_bucket(b"another bucket").unwrap();
        bucket.put(b"key", b"value".to_vec()).unwrap();
        bucket.put(b"keys", b"value".to_vec()).unwrap();
    }
    let mut bucket_names = vec![];
    tx.for_each(Box::new(|b, _v| -> Result<(), Error> {
        bucket_names.push(b.to_vec());
        Ok(())
    }))
    .unwrap();
    assert_eq!(bucket_names.len(), 2);
    assert!(bucket_names.contains(&b"bucket".to_vec()));
    assert!(bucket_names.contains(&b"another bucket".to_vec()));
}

#[test]
fn check() {
    let mut db = db_mock().build().unwrap();
    {
        let mut tx = db.begin_rw_tx().unwrap();
        {
            let mut bucket = tx.create_bucket(b"bucket").unwrap();
            bucket.put(b"key", b"value".to_vec()).unwrap();
            bucket.put(b"keys", b"value".to_vec()).unwrap();
        }
        {
            let mut bucket = tx.create_bucket(b"another bucket").unwrap();
            bucket.put(b"key", b"value".to_vec()).unwrap();
            bucket.put(b"keys", b"value".to_vec()).unwrap();
        }
        tx.commit().unwrap();
    }
    {
        let mut tx = db.begin_rw_tx().unwrap();
        tx.commit().unwrap();
    }
    {
        let tx = db.begin_tx().unwrap();
        tx.check_sync().unwrap();
        tx.rollback().unwrap();
    }
}

#[test]
fn check_corrupted() {
    let db = DBBuilder::new("./test_data/remark_fail.db")
        .read_only(true)
        .build()
        .unwrap();
    db.begin_tx().unwrap().check_sync().unwrap_err();
}

#[test]
fn drop_while_panicking_rolls_back() {
    let mut db = db_mock().build().unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut tx = db.begin_rw_tx().unwrap();
        let _ = tx.create_bucket(b"bucket").unwrap();
        panic!("user code failed");
    }));
    assert!(result.is_err());
    let tx = db.begin_tx().unwrap();
    assert!(tx.bucket(b"bucket").is_err());
}

#[test]
fn failed_commit_rolls_back_and_closes() {
    let mut db = db_mock().checkmode(CheckMode::NO).build().unwrap();
    {
        let mut tx = db.begin_rw_tx().unwrap();
        let _ = tx.create_bucket(b"bucket").unwrap();

        let inner = tx.0.clone();
        let meta = inner.meta.read();
        assert_eq!(tx.commit(), Err(Error::lock("meta")));
        drop(meta);
        drop(inner);

        assert!(tx.commit().is_err());
    }
    let tx = db.begin_rw_tx().unwrap();
    assert!(tx.bucket(b"bucket").is_err());
}

#[test]
fn check_reports_errors_instead_of_panicking() {
    let db = db_mock().build().unwrap();
    let tx = db.begin_tx().unwrap();

    let inner = tx.0.clone();
    let root = inner.root.write();
    let errors: Vec<String> = tx.check().iter().collect();
    drop(root);

    assert_eq!(errors, vec!["Lock failed: bucket".to_string()]);
}

#[test]
fn debug_with_locked_fields() {
    let db = db_mock().build().unwrap();
    let tx = db.begin_tx().unwrap();

    let inner = tx.0.clone();
    let root = inner.root.write();
    let debug = format!("{:?}", *inner);
    drop(root);

    assert!(debug.contains("root: RwLock { data: <locked>"), "{debug}");
}

#[test]
fn create_bucket_returns_bucket_errors() {
    let mut db = db_mock().build().unwrap();
    let mut tx = db.begin_rw_tx().unwrap();
    let _ = tx.create_bucket(b"bucket").unwrap();

    assert_eq!(tx.create_bucket(b"bucket").err(), Some(Error::BucketExists));
    assert_eq!(tx.create_bucket(b"").err(), Some(Error::NameRequired));
    assert_eq!(
        tx.create_bucket_if_not_exists(b"").err(),
        Some(Error::NameRequired)
    );
}

#[test]
fn closed_tx_reports_closed() {
    let mut db = db_mock().build().unwrap();
    {
        let mut tx = db.begin_rw_tx().unwrap();
        let mut bucket = tx.create_bucket(b"data").unwrap();
        bucket.put(b"key", b"value".to_vec()).unwrap();
    }
    assert!(!tx_mock().opened());

    let tx = db.begin_tx().unwrap();
    assert!(tx.opened());
    assert!(tx.cursor().first().unwrap().key.is_some());
    tx.rollback().unwrap();
    assert!(!tx.opened());
    assert_eq!(tx.cursor().first().err(), Some(Error::TxClosed));
}
