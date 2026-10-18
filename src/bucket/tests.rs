use crate::db::tests::db_mock;
use crate::errors::Error;
use crate::test_utils::test_seed;
use crate::tx::WeakTx;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{RngExt, SeedableRng};
use std::collections::{BTreeMap, BTreeSet};

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

#[test]
fn open_bucket_checks_value() {
    let bucket = bucket_mock(WeakTx::new());
    assert_eq!(
        bucket.open_bucket(vec![0; 8]).err(),
        Some(Error::CheckFail(vec![
            "Bucket value too short: 8".to_string()
        ]))
    );

    let mut value = vec![0u8; 16];
    value.extend(0u64.to_ne_bytes());
    value.extend(2u16.to_ne_bytes());
    value.extend(1u16.to_ne_bytes());
    value.extend(0u32.to_ne_bytes());
    for field in [0u32, 16, 1000, 0] {
        value.extend(field.to_ne_bytes());
    }
    let child = bucket.open_bucket(value).unwrap();
    assert_eq!(
        child
            .page_node(0)
            .unwrap()
            .upgrade()
            .left()
            .unwrap()
            .leaf_key(0)
            .err(),
        Some(Error::CheckFail(vec![
            "Page 0: data out of bounds".to_string()
        ]))
    );
}

#[test]
fn delete_many_keys_rebalances() {
    let mut db = db_mock().build().unwrap();
    let n: u64 = 20_000;
    {
        let mut tx = db.begin_rw_tx().unwrap();
        let mut b = tx.create_bucket(b"data").unwrap();
        for i in 0..n {
            b.put(&i.to_be_bytes(), vec![7; 64]).unwrap();
        }
        drop(b);
        tx.commit().unwrap();
    }
    for round in 0..4u64 {
        let mut tx = db.begin_rw_tx().unwrap();
        let mut b = tx.bucket_mut(b"data").unwrap();
        for i in (round..n).step_by(4) {
            b.delete(&i.to_be_bytes()).unwrap();
        }
        drop(b);
        tx.commit().unwrap();
    }
    let tx = db.begin_tx().unwrap();
    let b = tx.bucket(b"data").unwrap();
    for i in 0..n {
        assert!(b.try_get(&i.to_be_bytes()).unwrap().is_none(), "key {i}");
    }
}

#[test]
fn delete_random_order() {
    let seed = test_seed();
    let mut rng = StdRng::seed_from_u64(seed);
    let n: u64 = 20_000;
    let mut db = db_mock().page_size(1024).build().unwrap();
    {
        let mut tx = db.begin_rw_tx().unwrap();
        let mut b = tx.create_bucket(b"data").unwrap();
        for i in 0..n {
            b.put(&i.to_be_bytes(), vec![7; 64]).unwrap();
        }
        drop(b);
        tx.commit().unwrap();
    }

    let mut keys: Vec<u64> = (0..n).collect();
    keys.shuffle(&mut rng);
    let mut alive: BTreeSet<u64> = (0..n).collect();
    for (round, chunk) in keys.chunks(4_000).enumerate() {
        let mut tx = db.begin_rw_tx().unwrap();
        let mut b = tx.bucket_mut(b"data").unwrap();
        for key in chunk {
            b.delete(&key.to_be_bytes()).unwrap();
            alive.remove(key);
        }
        drop(b);
        tx.commit()
            .unwrap_or_else(|e| panic!("seed {seed}: commit after round {round}: {e}"));

        let tx = db.begin_tx().unwrap();
        let b = tx.bucket(b"data").unwrap();
        for key in 0..n {
            assert_eq!(
                b.try_get(&key.to_be_bytes()).unwrap().is_some(),
                alive.contains(&key),
                "seed {seed}: key {key} after round {round}"
            );
        }
    }
}

#[test]
fn random_puts_and_deletes_match_model() {
    let seed = test_seed();
    let mut rng = StdRng::seed_from_u64(seed);
    let mut db = db_mock().page_size(1024).build().unwrap();
    {
        let mut tx = db.begin_rw_tx().unwrap();
        let _ = tx.create_bucket(b"data").unwrap();
        tx.commit().unwrap();
    }

    let mut model = BTreeMap::<u64, Vec<u8>>::new();
    for round in 0..40 {
        let delete_chance = if round < 20 { 1 } else { 3 };
        let mut tx = db.begin_rw_tx().unwrap();
        let mut b = tx.bucket_mut(b"data").unwrap();
        for _ in 0..500 {
            let key = rng.random_range(0..5_000u64);
            if rng.random_range(0..4) < delete_chance {
                b.delete(&key.to_be_bytes()).unwrap();
                model.remove(&key);
            } else {
                let len = if rng.random_range(0..50) == 0 {
                    5_000
                } else {
                    rng.random_range(1..200)
                };
                let value = vec![rng.random_range(0..=255u8); len];
                b.put(&key.to_be_bytes(), value.clone()).unwrap();
                model.insert(key, value);
            }
        }
        drop(b);
        tx.commit()
            .unwrap_or_else(|e| panic!("seed {seed}: commit after round {round}: {e}"));

        let tx = db.begin_tx().unwrap();
        let b = tx.bucket(b"data").unwrap();
        for key in 0..5_000u64 {
            assert_eq!(
                b.try_get(&key.to_be_bytes()).unwrap(),
                model.get(&key).map(|v| v.as_slice()),
                "seed {seed}: key {key} after round {round}"
            );
        }
    }
}
