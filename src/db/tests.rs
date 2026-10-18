use super::common::{Batch, Call};
use super::{CheckMode, DBBuilder, WeakDB};
use crate::consts::{MAGIC, MAX_MAP_SIZE};
use crate::errors::Error;
use crate::test_utils::temp_file;
use crate::tx::Tx;
use std::sync::{Arc, mpsc};
use std::time::Duration;

#[expect(clippy::extra_unused_lifetimes)]
pub(crate) fn db_mock<'a>() -> DBBuilder {
    DBBuilder::new(temp_file())
        .autoremove(true)
        .read_only(false)
        .checkmode(CheckMode::PARANOID)
}

#[test]
fn open() {
    let db = db_mock().build().unwrap();
    assert_eq!(db.page_size(), page_size::get());
    assert_eq!(db.meta().unwrap().version, 2);
    assert_eq!(db.meta().unwrap().magic, MAGIC);
    assert_eq!(db.meta().unwrap().root.root, 3);
}

#[test]
fn mmap_size() {
    let db = db_mock().build().unwrap();
    assert_eq!(db.mmap_size(0).unwrap(), 1 << 15);
    assert_eq!(db.mmap_size(1 << 15).unwrap(), 1 << 15);
    assert_eq!(db.mmap_size((1 << 15) + 1).unwrap(), 1 << 16);
    assert_eq!(db.mmap_size(1 << 30).unwrap(), 1 << 30);
    assert_eq!(
        db.mmap_size((1 << 30) + 1).unwrap(),
        u64::min(2 << 30, MAX_MAP_SIZE)
    );
    assert_eq!(
        db.mmap_size((3 << 30) + 1).unwrap(),
        u64::min(4 << 30, MAX_MAP_SIZE)
    );
    assert_eq!(db.mmap_size(MAX_MAP_SIZE).unwrap(), MAX_MAP_SIZE);
    assert!(db.mmap_size(MAX_MAP_SIZE + 1).is_err());
}

#[test]
fn open_with_page_size() {
    let db = db_mock().page_size(4096).build().unwrap();
    assert_eq!(db.page_size(), 4096);
}

#[test]
fn open_existing() {
    let db = DBBuilder::new("./test_data/remark.db")
        .read_only(true)
        .build()
        .unwrap();
    assert_eq!(db.page_size(), 4096);
    assert_eq!(db.meta().unwrap().version, 2);
    assert_eq!(db.meta().unwrap().magic, MAGIC);
    assert_eq!(db.meta().unwrap().root.root, 9);
    {
        let tx = db.begin_tx().unwrap();
        let buckets = tx.buckets();
        assert_eq!(buckets.len(), 7);
        tx.check_sync().unwrap();
    }
}

#[test]
fn open_existing_2() {
    let db = DBBuilder::new("./test_data/freelist.db")
        .read_only(true)
        .build()
        .unwrap();
    assert_eq!(db.page_size(), 4096);
    assert_eq!(db.meta().unwrap().version, 2);
    assert_eq!(db.meta().unwrap().magic, MAGIC);
    assert_eq!(db.meta().unwrap().root.root, 3);
    db.meta().unwrap().validate().unwrap();
    {
        let tx = db.begin_tx().unwrap();
        let buckets = tx.buckets();
        assert_eq!(buckets.len(), 13);
        tx.check_sync().unwrap();
    }
}

#[test]
fn panic_while_update() {
    let mut db = db_mock().build().unwrap();

    // create bucket
    db.update(Box::new(|tx| {
        let _ = tx.create_bucket(b"exists").unwrap();
        Ok(())
    }))
    .unwrap();

    // ensure bucket exists
    db.view(Box::new(|tx| {
        assert!(tx.bucket(b"exists").is_ok());
        Ok(())
    }))
    .unwrap();

    // panicking
    db.update(Box::new(|tx| {
        let _ = tx.create_bucket(b"not exists").unwrap();
        panic!("oh shi!");
    }))
    .unwrap_err();

    // ensure transaction wasn't commited
    db.view(Box::new(|tx| {
        assert!(tx.bucket(b"exists").is_ok());
        assert!(tx.bucket(b"not exists").is_err());
        Ok(())
    }))
    .unwrap();
}

#[test]
fn batch() {
    use std::time::Duration;

    let db = db_mock()
        .batch_delay(Duration::from_secs(2))
        .batch_size(3)
        .build()
        .unwrap();

    let mut handles = vec![];
    for i in 0..10 {
        let mut db = db.clone();
        handles.push(std::thread::spawn(move || {
            db.batch(Box::new(move |tx| {
                let _ = tx.create_bucket(format!("{i}bubu").as_bytes()).unwrap();
                Ok(())
            }))
            .unwrap();
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    db.view(Box::new(|tx| {
        for i in 0..10 {
            let _ = tx.bucket(format!("{i}bubu").as_bytes()).unwrap();
        }
        Ok(())
    }))
    .unwrap();
}

#[test]
fn batch_returns_handler_error() {
    use std::time::Duration;

    let mut db = db_mock()
        .batch_delay(Duration::from_millis(10))
        .batch_size(1)
        .build()
        .unwrap();

    let result = db.batch(Box::new(|_tx| Err("Handler failed".to_string())));
    assert_eq!(result, Err(Error::Unexpected("Handler failed".to_string())));
}

fn batch_call(
    handler: impl Fn(&mut Tx) -> Result<(), String> + Send + 'static,
) -> (Call, mpsc::Receiver<Result<(), String>>) {
    let (sender, receiver) = mpsc::channel();
    (Call::new(Arc::new(Box::new(handler)), sender), receiver)
}

#[test]
fn batch_run_retries_without_failed_call() {
    let db = db_mock().build().unwrap();
    let mut batch = Batch::new(WeakDB::from(&db), 0, Duration::from_secs(60));

    let mut receivers = vec![];
    for name in ["a", "fail", "c"] {
        let (call, receiver) = batch_call(move |tx| {
            if name == "fail" {
                return Err("Handler failed".to_string());
            }
            let _ = tx.create_bucket(name.as_bytes())?;
            Ok(())
        });
        batch.push(call).unwrap();
        receivers.push(receiver);
    }
    batch.trigger();

    let results: Vec<_> = receivers.iter().map(|r| r.recv().unwrap()).collect();
    assert_eq!(
        results,
        vec![
            Ok(()),
            Err("Unexpected: Handler failed".to_string()),
            Ok(())
        ]
    );

    let tx = db.begin_tx().unwrap();
    assert!(tx.bucket(b"a").is_ok());
    assert!(tx.bucket(b"fail").is_err());
    assert!(tx.bucket(b"c").is_ok());
}

#[test]
fn batch_run_without_database() {
    let mut batch = Batch::new(WeakDB::new(), 0, Duration::from_secs(60));
    let (call, receiver) = batch_call(|_| Ok(()));
    batch.push(call).unwrap();
    batch.trigger();

    assert_eq!(receiver.recv().unwrap(), Err("Database gone".to_string()));
}

#[test]
fn batch_push_after_run() {
    let mut batch = Batch::new(WeakDB::new(), 0, Duration::from_secs(60));
    batch.trigger();

    let (call, _receiver) = batch_call(|_| Ok(()));
    assert_eq!(
        batch.push(call),
        Err(Error::Unexpected("Batch already run".to_string()))
    );
}
