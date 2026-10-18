use fnv::FnvHashSet;
use parking_lot::{
    MappedRwLockReadGuard, MappedRwLockWriteGuard, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard,
};
use std::collections::HashMap;
use std::fmt;
use std::fs::OpenOptions;
use std::io::{Cursor as IOCursor, Read, Seek, SeekFrom, Write, copy};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak, mpsc};
use std::thread;
use std::time::{Duration, SystemTime};

use crate::bucket::{Bucket, Cursor};
use crate::consts::{Flags, IGNORE_NOSYNC, PGID, TXID};
use crate::db::{CheckMode, DB, WeakDB};
use crate::errors::Error;
use crate::meta::Meta;
use crate::page::{OwnedPage, Page, PageInfo};

use super::stats::TxStats;

pub(crate) struct TxInner {
    /// is transaction writable
    pub(crate) writable: bool,

    /// declares that transaction is in use
    pub(crate) managed: AtomicBool,

    /// defines whether transaction will be checked on close
    pub(crate) check: AtomicBool,

    /// ref to DB.
    /// if transaction closed then ref points to null
    pub(crate) db: RwLock<WeakDB>,

    /// transaction meta
    pub(crate) meta: RwLock<Meta>,

    /// root bucket.
    /// this bucket holds another buckets
    pub(crate) root: RwLock<Bucket>,

    /// pages cache
    pub(crate) pages: RwLock<HashMap<PGID, OwnedPage>>,
    /// ids of mmap pages already bounds-checked
    pub(crate) checked_pages: RwLock<FnvHashSet<PGID>>,

    /// transactions statistics
    pub(crate) stats: Mutex<TxStats>,

    /// list of callbacks that will be called after commit
    pub(crate) commit_handlers: Mutex<Vec<Box<dyn Fn()>>>,

    /// WriteFlag specifies the flag for write-related methods like WriteTo().
    /// Tx opens the database file with the specified flag to copy the data.
    ///
    /// By default, the flag is unset, which works well for mostly in-memory
    /// workloads. For databases that are much larger than available RAM,
    /// set the flag to syscall.O_DIRECT to avoid trashing the page cache.
    pub(super) write_flag: usize,
}

impl fmt::Debug for TxInner {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let db = self.db.try_read().map(|db| db.as_ptr());
        let commit_handlers = self.commit_handlers.try_lock().map(|h| h.len());

        f.debug_struct("TxInner")
            .field("writable", &self.writable)
            .field("managed", &self.managed)
            .field("db", &db)
            .field("meta", &self.meta)
            .field("root", &self.root)
            .field("pages", &self.pages)
            .field("stats", &self.stats)
            .field("commit handlers len", &commit_handlers)
            .field("write_flag", &self.write_flag)
            .finish()
    }
}

/// Transaction
#[derive(Debug)]
pub struct Tx(pub(crate) Arc<TxInner>);

unsafe impl Sync for Tx {}
unsafe impl Send for Tx {}

impl Tx {
    pub(crate) fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }

    /// Returns whether the transaction can perform write operations.
    pub fn writable(&self) -> bool {
        self.0.writable
    }

    /// Defines whether transaction is fresh and not been used before
    /// commiting transaction twice must resolve in error
    pub(crate) fn opened(&self) -> Result<bool, Error> {
        match self
            .0
            .db
            .try_read()
            .ok_or_else(|| Error::lock("db"))?
            .upgrade()
        {
            None => Ok(false),
            Some(db) => Ok(db.opened()),
        }
    }

    /// Returns a reference to the database that created the transaction.
    pub(crate) fn db(&self) -> Result<DB, Error> {
        self.0
            .db
            .try_read()
            .ok_or(Error::TxClosed)?
            .upgrade()
            .ok_or(Error::DatabaseGone)
    }

    /// Returns the transaction id.
    pub(crate) fn id(&self) -> Result<TXID, Error> {
        Ok(self
            .0
            .meta
            .try_read()
            .ok_or_else(|| Error::lock("meta"))?
            .txid)
    }

    /// Returns the transaction page id.
    pub(crate) fn pgid(&self) -> Result<PGID, Error> {
        Ok(self
            .0
            .meta
            .try_read()
            .ok_or_else(|| Error::lock("meta"))?
            .pgid)
    }

    /// Returns the transaction page id.
    pub(crate) fn set_pgid(&mut self, id: PGID) -> Result<(), Error> {
        self.0
            .meta
            .try_write()
            .ok_or_else(|| Error::lock("meta"))?
            .pgid = id;
        Ok(())
    }

    /// Adds a handler function to be executed after the transaction successfully commits.
    pub fn on_commit(&mut self, handler: Box<dyn Fn()>) {
        self.0.commit_handlers.lock().push(handler);
    }

    /// Returns current database size in bytes as seen by this transaction.
    pub(super) fn size(&self) -> Result<i64, Error> {
        Ok(self.pgid()? as i64 * self.db()?.page_size() as i64)
    }

    /// Creates a cursor associated with the root bucket.
    /// All items in the cursor will return a nil value because all items in root bucket are also buckets.
    /// The cursor is only valid as long as the transaction is open.
    /// Do not use a cursor after the transaction is closed.
    pub fn cursor(&self) -> Cursor<'_, RwLockWriteGuard<'_, Bucket>> {
        self.0.stats.lock().cursor_count += 1;

        Cursor::new(self.0.root.write())
    }

    // Returns a copy of the current transaction statistics.
    pub fn stats(&self) -> TxStats {
        self.0.stats.lock().clone()
    }

    /// Bucket retrieves a bucket by name.
    /// Returns None if the bucket does not exist.
    /// The bucket instance is only valid for the lifetime of the transaction.
    pub fn bucket(&self, key: &[u8]) -> Result<MappedRwLockReadGuard<'_, Bucket>, Error> {
        let bucket = self
            .0
            .root
            .try_read()
            .ok_or_else(|| Error::lock("bucket"))?;

        let mut error = None;
        RwLockReadGuard::try_map(bucket, |b| {
            b.try_bucket(key).unwrap_or_else(|e| {
                error = Some(e);
                None
            })
        })
        .map_err(|_| error.unwrap_or_else(|| "Can't get bucket".into()))
    }

    /// Bucket retrieves a mutable bucket by name.
    /// Returns None if the bucket does not exist.
    /// The bucket instance is only valid for the lifetime of the transaction.
    pub fn bucket_mut(&mut self, key: &[u8]) -> Result<MappedRwLockWriteGuard<'_, Bucket>, Error> {
        if !self.0.writable {
            return Err(Error::TxReadonly);
        };

        let bucket = self
            .0
            .root
            .try_write()
            .ok_or_else(|| Error::lock("bucket"))?;

        let mut error = None;
        RwLockWriteGuard::try_map(bucket, |b| {
            b.try_bucket_mut(key).unwrap_or_else(|e| {
                error = Some(e);
                None
            })
        })
        .map_err(|_| error.unwrap_or_else(|| "Can't get bucket".into()))
    }

    /// returns bucket keys for db
    pub fn buckets(&self) -> Vec<Vec<u8>> {
        self.try_buckets().unwrap()
    }

    /// Same as [`Tx::buckets`], but returns an error instead of panicking.
    pub fn try_buckets(&self) -> Result<Vec<Vec<u8>>, Error> {
        self.0.root.read().try_buckets()
    }

    /// Creates a new bucket.
    /// Returns an error if the bucket already exists, if the bucket name is blank, or if the bucket name is too long.
    /// The bucket instance is only valid for the lifetime of the transaction.
    pub fn create_bucket(
        &mut self,
        key: &[u8],
    ) -> Result<MappedRwLockWriteGuard<'_, Bucket>, Error> {
        if !self.0.writable {
            return Err(Error::TxReadonly);
        };

        let bucket = self
            .0
            .root
            .try_write()
            .ok_or_else(|| Error::lock("bucket"))?;

        RwLockWriteGuard::try_map(bucket, |b| b.create_bucket(key).ok())
            .map_err(|_| "Can't get bucket".into())
    }

    /// Creates a new bucket if it doesn't already exist.
    /// Returns an error if the bucket name is blank, or if the bucket name is too long.
    /// The bucket instance is only valid for the lifetime of the transaction.
    pub fn create_bucket_if_not_exists(
        &mut self,
        key: &[u8],
    ) -> Result<MappedRwLockWriteGuard<'_, Bucket>, Error> {
        if !self.0.writable {
            return Err(Error::TxReadonly);
        };

        let bucket = self
            .0
            .root
            .try_write()
            .ok_or_else(|| Error::lock("bucket"))?;

        RwLockWriteGuard::try_map(bucket, |b| b.create_bucket_if_not_exists(key).ok())
            .map_err(|_| "Can't get bucket".into())
    }

    /// Deletes a bucket.
    /// Returns an error if the bucket cannot be found or if the key represents a non-bucket value.
    pub fn delete_bucket(&mut self, key: &[u8]) -> Result<(), Error> {
        if !self.0.writable {
            return Err(Error::TxReadonly);
        };

        self.0
            .root
            .try_write()
            .ok_or_else(|| Error::lock("bucket"))?
            .delete_bucket(key)
    }

    #[expect(clippy::type_complexity)]
    /// Executes a function for each bucket in the root.
    /// If the provided function returns an error then the iteration is stopped and
    /// the error is returned to the caller.
    ///
    /// first argument of function is bucket's key, second is bucket itself
    pub fn for_each<'a, E: Into<Error>>(
        &self,
        mut handler: Box<dyn FnMut(&[u8], Option<&Bucket>) -> Result<(), E> + 'a>,
    ) -> Result<(), Error> {
        let root = self
            .0
            .root
            .try_write()
            .ok_or_else(|| Error::lock("bucket"))?;
        root.for_each(Box::new(
            |k: &[u8], _v: Option<&[u8]>| -> Result<(), Error> {
                handler(k, root.try_bucket(k)?).map_err(Into::into)
            },
        ))
    }

    /// Writes the entire database to a writer.
    /// If err == nil then exactly tx.Size() bytes will be written into the writer.
    pub fn write_to<W: Write>(&self, mut w: W) -> Result<i64, Error> {
        let db = self.db()?;
        let page_size = db.page_size();
        let mut file = db.0.file.try_write().ok_or_else(|| Error::lock("file"))?;

        let mut written = 0;

        let mut page = OwnedPage::new(page_size);
        page.flags = Flags::META;

        // first page
        {
            *page.meta_mut()? = self
                .0
                .meta
                .try_read()
                .ok_or_else(|| Error::lock("meta"))?
                .clone();
            page.meta_mut()?.checksum = page.meta()?.sum64();
            w.write_all(page.buf()).map_err(|e| format!("{e}"))?;
            written += page.size();
        }

        // second page
        {
            page.id = 1;
            page.meta_mut()?.txid -= 1;
            page.meta_mut()?.checksum = page.meta()?.sum64();
            w.write_all(page.buf()).map_err(|e| format!("{e}"))?;
            written += page.size();
        }

        file.seek(SeekFrom::Start(page_size as u64 * 2))
            .map_err(|e| format!("{e}"))?;

        let size = self.size()? as u64 - (page_size as u64 * 2);
        written += copy(&mut Read::by_ref(&mut file.get_mut()).take(size), &mut w)
            .map_err(|e| format!("{e}"))? as usize;

        Ok(written as i64)
    }

    /// Copies the entire database to file at the given path.
    /// A reader transaction is maintained during the copy so it is safe to continue
    /// using the database while a copy is in progress.
    pub fn copy_to(&self, path: &str, mode: OpenOptions) -> Result<(), Error> {
        let file = mode.open(path).map_err(|e| e.to_string())?;
        self.write_to(file)?;
        Ok(())
    }

    /// Closes transaction (so subsequent use of it will resolve in error)
    pub(crate) fn close(&self) -> Result<(), Error> {
        let mut db = self.db()?;
        let tx = db.remove_tx(self)?;

        *tx.0.db.write() = WeakDB::new();
        tx.0.root
            .try_write()
            .ok_or_else(|| Error::lock("bucket"))?
            .clear();
        tx.0.pages
            .try_write()
            .ok_or_else(|| Error::lock("pages"))?
            .clear();
        Ok(())
    }

    /// Writes all changes to disk and updates the meta page.
    /// Returns an error if a disk write error occurs, or if Commit is
    /// called on a read-only transaction.
    pub fn commit(&mut self) -> Result<(), Error> {
        if self.0.managed.load(Ordering::Acquire) {
            return Err(Error::TxManaged);
        } else if !self.writable() {
            return Err(Error::TxReadonly);
        };

        let result = self.commit_inner();
        if result.is_err() && self.db().is_ok() {
            self.rollback_inner()?;
        }
        result
    }

    fn commit_inner(&mut self) -> Result<(), Error> {
        let mut db = self.db()?;

        {
            let start_time = SystemTime::now();
            self.0
                .root
                .try_write()
                .ok_or_else(|| Error::lock("bucket"))?
                .rebalance()?;
            let mut stats = self.0.stats.lock();
            if stats.rebalance > 0 {
                stats.rebalance_time += SystemTime::now().duration_since(start_time)?;
            };
        }

        {
            // spill
            let start_time = SystemTime::now();
            self.0
                .root
                .try_write()
                .ok_or_else(|| Error::lock("bucket"))?
                .spill()?;
            self.0
                .stats
                .try_lock()
                .ok_or_else(|| Error::lock("stats"))?
                .spill_time = SystemTime::now().duration_since(start_time)?;
        }

        // Free the old root bucket.
        let root = self
            .0
            .root
            .try_read()
            .ok_or_else(|| Error::lock("bucket"))?
            .bucket
            .root;
        self.0
            .meta
            .try_write()
            .ok_or_else(|| Error::lock("meta"))?
            .root
            .root = root;

        let (txid, tx_pgid, freelist_pgid) = {
            let meta = self.0.meta.try_read().ok_or_else(|| Error::lock("meta"))?;
            (meta.txid as usize, meta.pgid, meta.freelist)
        };

        let (freelist_size, page_size) = {
            // Free the freelist and allocate new pages for it. This will overestimate
            // the size of the freelist but not underestimate the size (which would be bad).
            let page = db.page(freelist_pgid)?;
            let mut freelist =
                db.0.freelist
                    .try_write()
                    .ok_or_else(|| Error::lock("freelist"))?;
            freelist.free(txid as u64, &page)?;

            let freelist_size = freelist.size();
            let page_size = db.page_size();

            (freelist_size, page_size)
        };

        {
            let page = self.allocate((freelist_size / page_size) as u64 + 1)?;
            let page = unsafe { &mut *page };

            db.0.freelist
                .try_write()
                .ok_or_else(|| Error::lock("freelist"))?
                .write(page)?;
            self.0
                .meta
                .try_write()
                .ok_or_else(|| Error::lock("meta"))?
                .freelist = page.id;

            // If the high water mark has moved up then attempt to grow the database.
            if self.pgid()? > tx_pgid {
                db.grow((tx_pgid + 1) * page_size as u64)?;
            }

            // Write dirty pages to disk.
            let write_start_time = SystemTime::now();
            self.write()?;

            // If strict mode is enabled then perform a consistency check.
            // Only the first consistency error is reported in the panic.
            if self.0.check.swap(false, Ordering::AcqRel) {
                let strict = db.0.check_mode.contains(CheckMode::STRICT);
                if let Err(e) = self.check_sync() {
                    if strict {
                        return Err(e);
                    } else {
                        println!("{e}");
                    }
                }
            };

            // Write meta to disk.
            if let Err(e) = self.write_meta() {
                self.0.check.store(false, Ordering::Release);
                return Err(e);
            };

            self.0
                .stats
                .try_lock()
                .ok_or_else(|| Error::lock("stats"))?
                .write_time += SystemTime::now().duration_since(write_start_time)?;
        };

        // Finalize the transaction.
        self.close()?;

        {
            for h in &*self
                .0
                .commit_handlers
                .try_lock()
                .ok_or_else(|| Error::lock("commit handlers"))?
            {
                h();
            }
        }

        Ok(())
    }

    /// Closes the transaction and ignores all previous updates. Read-only
    /// transactions must be rolled back and not committed.
    pub fn rollback(&self) -> Result<(), Error> {
        if self.0.managed.load(Ordering::Acquire) {
            return Err(Error::TxManaged);
        };
        self.rollback_inner()?;
        Ok(())
    }

    pub(crate) fn rollback_inner(&self) -> Result<(), Error> {
        let db = self.db()?;
        if self.0.check.swap(false, Ordering::AcqRel) {
            let strict = db.0.check_mode.contains(CheckMode::STRICT);
            if let Err(e) = self.check_sync() {
                if strict {
                    return Err(e);
                } else {
                    println!("{e}");
                }
            }
        };
        if self.writable() {
            let txid = self.id()?;
            let mut freelist = db.0.freelist.write();
            freelist.rollback(txid)?;
            let freelist_id = db.meta()?.freelist;
            let freelist_page = db.page(freelist_id)?;
            freelist.reload(&freelist_page)?;
        };
        self.close()?;
        Ok(())
    }

    /// Sync version of check()
    ///
    /// In case of checking thread panic will also return Error
    pub fn check_sync(&self) -> Result<(), Error> {
        let (sender, ch) = mpsc::channel::<String>();
        let tx = self.clone();
        let handle = thread::spawn(move || tx.check_into(sender));

        let mut errs = vec![];
        for err in ch {
            errs.push(err);
        }

        if let Err(e) = handle.join() {
            let estr = e.downcast_ref::<String>();
            if let Some(estr) = estr {
                errs.push(estr.clone());
            } else {
                errs.push(format!("Check thread panicked: {e:?}"));
            }
        }

        if !errs.is_empty() {
            return Err(Error::CheckFail(errs));
        };

        Ok(())
    }

    // Performs several consistency checks on the database for this transaction.
    // An error is returned if any inconsistency is found.
    //
    // It can be safely run concurrently on a writable transaction. However, this
    // incurs a high cost for large databases and databases with a lot of subbuckets
    // because of caching. This overhead can be removed if running on a read-only
    // transaction, however, it is not safe to execute other writer transactions at
    // the same time.
    pub fn check(&self) -> mpsc::Receiver<String> {
        let (sender, receiver) = mpsc::channel::<String>();
        let tx = self.clone();
        thread::spawn(move || tx.check_into(sender));
        receiver
    }

    pub fn freed(&self) -> Result<HashMap<PGID, bool>, Error> {
        let mut freed = HashMap::<PGID, bool>::new();
        let all_pgids = self
            .db()?
            .0
            .freelist
            .try_read()
            .ok_or_else(|| Error::lock("freelist"))?
            .get_pgids();

        for id in &all_pgids {
            if freed.contains_key(id) {
                return Err(format!("Page {id}: already freed").into());
            }
            freed.insert(*id, true);
        }

        Ok(freed)
    }

    pub(super) fn check_into(&self, ch: mpsc::Sender<String>) {
        if let Err(e) = self.check_pages(&ch) {
            ch.send(e.to_string()).expect("Check receiver dropped");
        }
    }

    fn check_pages(&self, ch: &mpsc::Sender<String>) -> Result<(), Error> {
        let freed = self.freed()?;

        let mut reachable = HashMap::new();
        reachable.insert(0, true);
        reachable.insert(1, true);
        let freelist_pgid = self
            .0
            .meta
            .try_read_for(Duration::from_secs(10))
            .ok_or_else(|| Error::lock("meta"))?
            .freelist;
        let freelist_overflow = unsafe { &*self.page(freelist_pgid)? }.overflow;
        for i in 0..=freelist_overflow {
            reachable.insert(freelist_pgid + u64::from(i), true);
        }

        let root = self
            .0
            .root
            .try_read()
            .ok_or_else(|| Error::lock("bucket"))?;
        self.check_bucket(&root, &mut reachable, &freed, ch)?;

        let pgid = self
            .0
            .meta
            .try_read()
            .ok_or_else(|| Error::lock("meta"))?
            .pgid;
        for i in 0..pgid {
            let is_reachable = reachable.contains_key(&i);
            let is_freed = freed.contains_key(&i);
            if !is_reachable && !is_freed {
                report(ch, format!("Page {i}: unreachable unfreed"))?;
            };
        }
        Ok(())
    }

    fn check_bucket(
        &self,
        b: &Bucket,
        reachable: &mut HashMap<PGID, bool>,
        freed: &HashMap<PGID, bool>,
        ch: &mpsc::Sender<String>,
    ) -> Result<(), Error> {
        if b.bucket.root == 0 {
            return Ok(());
        }

        let meta_pgid = self.pgid()?;

        let handler = Box::new(|p: &Page, _pgid: usize| {
            if p.id > meta_pgid {
                report(ch, format!("Page {}: out of bounds: {}", p.id, meta_pgid))?;
            }

            for i in 0..=p.overflow {
                let id = p.id + u64::from(i);
                if reachable.contains_key(&id) {
                    report(ch, format!("Page {id}: multiple references"))?;
                }
                reachable.insert(id, true);
            }

            let page_type_is_valid = matches!(p.flags, Flags::BRANCHES | Flags::LEAVES);

            if freed.contains_key(&p.id) {
                report(ch, format!("Page {}: reachable freed", p.id))?;
            } else if !page_type_is_valid {
                report(ch, format!("Page {}: invalid type: {}", p.id, p.flags))?;
            }
            Ok(())
        });

        b.tx()?.for_each_page(b.bucket.root, 0, handler)?;

        b.for_each(Box::new(|k, _v| -> Result<(), Error> {
            if let Some(child) = b.try_bucket(k)? {
                self.check_bucket(child, reachable, freed, ch)?;
            };
            Ok(())
        }))
    }

    /// Returns a contiguous block of memory starting at a given page.
    pub(crate) fn allocate(&mut self, count: u64) -> Result<*mut Page, Error> {
        let Ok(mut db) = self.db() else {
            return Err(Error::TxClosed);
        };

        let mut page = db.allocate(count, self)?;
        let page_id = page.id;
        let page_ptr = &raw mut *page;

        self.0
            .pages
            .try_write()
            .ok_or_else(|| Error::lock("pages"))?
            .insert(page_id, page);

        // Update statistics.
        {
            let mut stats = self.0.stats.lock();
            stats.page_count += 1;
            stats.page_alloc += count as usize * self.db()?.page_size();
        }

        Ok(page_ptr)
    }

    /// Writes any dirty pages to disk.
    pub(crate) fn write(&mut self) -> Result<(), Error> {
        let mut pages: Vec<_> = self
            .0
            .pages
            .write()
            .drain()
            .map(|x| {
                let pgid = x.1.id;
                (pgid, x.1)
            })
            .collect();
        pages.sort_by_key(|a| a.0);
        let mut db = self.db()?;

        let page_size = db.page_size();
        for (id, p) in &pages {
            let size = (p.overflow + 1) as usize * page_size;
            let offset = *id * page_size as u64;

            let buf = unsafe { std::slice::from_raw_parts(p.as_ptr(), size) };
            let cursor = IOCursor::new(buf);

            db.write_at(offset, cursor)?;
        }

        if !db.0.no_sync || IGNORE_NOSYNC {
            db.sync()?;
        }

        {
            let mut page_pool = db.0.page_pool.lock();
            let mut i = 0;
            while i != pages.len() {
                if pages[i].1.size() == page_size {
                    let mut page = pages.remove(i).1;
                    for i in page.buf_mut() {
                        *i = 0;
                    }
                    page_pool.push(page);
                } else {
                    i += 1;
                }
            }
        }

        Ok(())
    }

    /// Writes the meta to the disk.
    pub(crate) fn write_meta(&mut self) -> Result<(), Error> {
        let mut db = self.db()?;

        let mut buf = vec![0u8; db.page_size()];
        let page = Page::from_buf_mut(&mut buf);
        self.0
            .meta
            .try_write()
            .ok_or_else(|| Error::lock("meta"))?
            .write(page)?;

        db.write_at(0, IOCursor::new(buf))?;

        if !db.0.no_sync || IGNORE_NOSYNC {
            db.sync()?;
        }

        self.0.stats.lock().write += 1;
        Ok(())
    }

    /// Returns a reference to the page with a given id.
    /// If page has been written to then a temporary buffered page is returned.
    pub(crate) fn page(&self, id: PGID) -> Result<*const Page, Error> {
        // Check the dirty pages first.
        {
            let pages = self
                .0
                .pages
                .try_read()
                .ok_or_else(|| Error::lock("pages"))?;
            if let Some(p) = pages.get(&id) {
                return Ok(&raw const **p);
            }
        }

        // Otherwise return directly from the mmap.
        let db = self.db()?;
        if self.0.checked_pages.read().contains(&id) {
            return Ok(&raw const *db.page_span(id)?);
        }
        let page = db.page(id)?;
        self.0.checked_pages.write().insert(id);
        Ok(&raw const *page)
    }

    /// Iterates over every page within a given page and executes a function.
    pub(crate) fn for_each_page<'a>(
        &self,
        pgid: PGID,
        depth: usize,
        mut func: Box<dyn FnMut(&Page, usize) -> Result<(), Error> + 'a>,
    ) -> Result<(), Error> {
        let p = unsafe { &*self.page(pgid)? };

        // Execute function.
        func(p, depth)?;

        // Recursively loop over children.
        let flags = p.flags;
        if flags != Flags::BRANCHES {
            return Ok(());
        }
        let count = p.count as usize;
        for i in 0..count {
            let el = p.branch_page_element(i)?;
            self.for_each_page(el.pgid, depth + 1, Box::new(|p, d| func(p, d)))?;
        }
        Ok(())
    }

    /// Returns page information for a given page number.
    /// This is only safe for concurrent use when used by a writable transaction.
    pub fn page_info(&self, id: usize) -> Result<Option<PageInfo>, Error> {
        if !self.opened()? {
            return Err(Error::TxClosed);
        };
        if id >= self.pgid()? as usize {
            return Ok(None);
        };

        let db = self.db()?;

        // Build the page info.
        let p = db.page_header(id as u64)?;
        let mut info = PageInfo {
            id: id as isize,
            ptype: Flags::FREELIST,
            count: p.count as usize,
            overflow_count: p.overflow as usize,
        };

        // Determine the type (or if it's free).
        if !db
            .0
            .freelist
            .try_read()
            .ok_or_else(|| Error::lock("freelist"))?
            .freed(id as u64)
        {
            info.ptype = p.flags
        }

        Ok(Some(info))
    }
}

impl Drop for Tx {
    fn drop(&mut self) {
        let count = Arc::strong_count(&self.0);
        // one for the reference, and one that is owned by db
        if count > 2 {
            return;
        };
        if let Ok(_db) = self.db() {
            if self.0.writable && !std::thread::panicking() {
                self.commit().expect("Automatic commit failed");
            } else {
                self.rollback().expect("Automatic rollback failed");
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct WeakTx(Weak<TxInner>);

impl WeakTx {
    pub(crate) fn new() -> Self {
        Self(Weak::new())
    }

    pub(crate) fn upgrade(&self) -> Option<Tx> {
        self.0.upgrade().map(Tx)
    }

    pub(crate) fn from(tx: &Tx) -> Self {
        Self(Arc::downgrade(&tx.0))
    }
}

fn report(ch: &mpsc::Sender<String>, msg: String) -> Result<(), Error> {
    ch.send(msg).map_err(|_| "Check receiver dropped".into())
}
