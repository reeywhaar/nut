use crate::db::tests::db_mock;
use crate::errors::Error;
use crate::tx::WeakTx;

use super::Bucket;

pub(crate) fn bucket_mock(tx: WeakTx) -> Bucket {
    Bucket::new(tx)
}

#[test]
fn create() {
    let mut db = db_mock().build().unwrap();
    let mut tx = db.begin_rw_tx().unwrap();
    assert!(tx.bucket(b"foo").is_err());

    let mut bucket = tx.create_bucket(b"foo").unwrap();
    assert!(bucket.get(b"bar").is_none());

    bucket.put(b"foo", b"jaja".to_vec()).unwrap();
    assert!(bucket.get(b"foo").is_some());
    assert_eq!(bucket.get(b"foo").unwrap(), b"jaja");

    bucket.create_bucket(b"subbucket").unwrap();
    assert!(bucket.get(b"subbucket").is_none());
    assert!(bucket.bucket(b"subbucket").is_some());
}

#[test]
fn create_nested_buckets() {
    let path = {
        let mut db = db_mock().autoremove(false).build().unwrap();
        let mut tx = db.begin_rw_tx().unwrap();
        assert!(tx.bucket(b"foo").is_err());

        tx.create_bucket(b"foo")
            .unwrap()
            .create_bucket(b"foob")
            .unwrap()
            .create_bucket(b"fooc")
            .unwrap()
            .create_bucket(b"food")
            .unwrap()
            .create_bucket(b"fooe")
            .unwrap();

        assert!(
            tx.bucket(b"foo")
                .unwrap()
                .bucket(b"foob")
                .unwrap()
                .bucket(b"fooc")
                .unwrap()
                .bucket(b"food")
                .unwrap()
                .bucket(b"fooe")
                .is_some()
        );

        tx.commit().unwrap();
        db.path().to_owned()
    };

    let db = db_mock().path(path).build().unwrap();
    let tx = db.begin_tx().unwrap();
    assert!(
        tx.bucket(b"foo")
            .unwrap()
            .bucket(b"foob")
            .unwrap()
            .bucket(b"fooc")
            .unwrap()
            .bucket(b"food")
            .unwrap()
            .bucket(b"fooe")
            .is_some()
    );
}

#[test]
fn delete_value() {
    let mut db = db_mock().build().unwrap();
    let mut tx = db.begin_rw_tx().unwrap();
    {
        let mut bucket = tx.create_bucket(b"bucket").unwrap();
        bucket.put(b"haley", b"smith".to_vec()).unwrap();
        assert_eq!(bucket.get(b"haley").unwrap(), b"smith");
        bucket.delete(b"haley").unwrap();
        assert_eq!(bucket.get(b"haley"), None);
    }
    tx.commit().unwrap();
}

#[test]
fn delete_bucket_err() {
    let mut db = db_mock().build().unwrap();
    let mut tx = db.begin_rw_tx().unwrap();
    {
        let mut bucket = tx.create_bucket(b"bucket").unwrap();
        bucket.create_bucket(b"stan").unwrap();
        bucket.bucket(b"stan").unwrap();
        assert_eq!(bucket.delete(b"stan"), Err(Error::IncompatibleValue));
    }
    tx.commit().unwrap();
}

#[test]
fn delete_missing_bucket_after_last_key() {
    let mut db = db_mock().build().unwrap();
    let mut tx = db.begin_rw_tx().unwrap();
    let mut root = tx.create_bucket(b"root").unwrap();
    root.create_bucket(b"a").unwrap();
    assert_eq!(root.delete_bucket(b"b"), Err(Error::BucketNotFound));
}

#[test]
fn try_variants() {
    let mut db = db_mock().build().unwrap();
    {
        let mut tx = db.begin_rw_tx().unwrap();
        let mut root = tx.create_bucket(b"root").unwrap();
        root.put(b"key", b"value".to_vec()).unwrap();
        root.create_bucket(b"child").unwrap();

        assert_eq!(root.try_get(b"key"), Ok(Some(&b"value"[..])));
        assert_eq!(root.try_get(b"missing"), Ok(None));
        assert_eq!(root.try_get(b"child"), Ok(None));
        assert!(root.try_bucket(b"child").unwrap().is_some());
        assert!(root.try_bucket(b"missing").unwrap().is_none());
        assert!(root.try_bucket_mut(b"child").unwrap().is_some());
        assert_eq!(root.try_buckets(), Ok(vec![b"child".to_vec()]));
    }

    let tx = db.begin_tx().unwrap();
    let root = tx.bucket(b"root").unwrap();
    assert_eq!(
        format!("{:?}", root.try_stats().unwrap()),
        format!("{:?}", root.stats())
    );
}
