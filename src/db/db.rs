use bitflags::bitflags;
use fs4::FileExt;
use lock_api::{RawMutex, RawRwLock};
use parking_lot::{MappedRwLockReadGuard, Mutex, RwLock, RwLockReadGuard};
use std::fs::File;
use std::fs::OpenOptions;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak, mpsc};
use std::time::Duration;

use crate::bucket::IBucket;
use crate::consts::{Flags, MAGIC, MAX_MAP_SIZE, MAX_MMAP_STEP, PGID, VERSION};
use crate::errors::{Error, io};
use crate::freelist::FreeList;
use crate::meta::Meta;
use crate::page::{OwnedPage, Page};
use crate::tx::{Tx, TxBuilder};

use super::builder::Options;
use super::common::{Batch, Call};
use super::info::Info;
use super::stats::Stats;
use super::{RWTxGuard, TxGuard};

bitflags! {
    /// Defines when db check will occur
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct CheckMode: u8 {
        /// no check
        const NO = 0b0000;
        /// check on database close
        const CLOSE = 0b0001;
        /// check on end of read transaction
        ///
        /// If there is parallel write transaction going on
        /// check may fail because of old freelist metadata
        const READ = 0b0010;
        /// check on end of write transaction
        const WRITE = 0b0100;
        /// check on close, and end of every transaction
        const ALL = 0b0111;
        /// defines whether result of the check will result
        /// in error or just be spewed in stdout
        const STRICT = 0b1000;
        /// check on close and writes and panic on error
        const STRONG = 0b1101;
        /// check everything and panic on error
        const PARANOID = 0b1111;
    }
}

pub(crate) struct DBInner {
    pub(crate) check_mode: CheckMode,
    pub(crate) no_sync: bool,
    pub(super) no_grow_sync: bool,
    pub(super) max_batch_size: usize,
    pub(super) max_batch_delay: Duration,
    pub(super) autoremove: bool,
    pub(super) alloc_size: u64,
    pub(super) path: PathBuf,
    pub(crate) file: RwLock<BufWriter<File>>,
    pub(super) mmap_size: Mutex<usize>,
    pub(super) mmap: RwLock<memmap2::Mmap>,
    pub(super) file_size: RwLock<u64>,
    page_size: usize,
    opened: AtomicBool,
    pub(crate) rw_lock: Mutex<()>,
    pub(crate) rw_tx: RwLock<Option<Tx>>,
    pub(crate) txs: RwLock<Vec<Tx>>,
    pub(crate) freelist: RwLock<FreeList>,
    pub(crate) stats: RwLock<Stats>,
    pub(crate) batch: Mutex<Option<Batch>>,
    pub(crate) page_pool: Mutex<Vec<OwnedPage>>,
    read_only: bool,
}

/// Database
#[derive(Clone)]
pub struct DB(pub(crate) Arc<DBInner>);

impl<'a> DB {
    pub(super) fn open_file<P: Into<PathBuf>>(
        mut file: File,
        path: P,
        options: Options,
    ) -> Result<Self, Error> {
        let path = path.into();
        let needs_initialization =
            (!path.exists()) || file.metadata().map_err(io("Can't read metadata"))?.len() == 0;

        if !options.read_only {
            file.lock().map_err(io("Cannot lock db file"))?;
        } else {
            file.lock_shared().map_err(io("Cannot lock db file"))?;
        };

        let page_size = if needs_initialization {
            options.page_size
        } else {
            let mut buf = OwnedPage::new(1000);
            file.read_exact(buf.buf_mut())?;
            let page: &Page = &buf;
            if page.flags != Flags::META {
                return Err(Error::Invalid);
            }
            page.meta()?.page_size as usize
        };
        let page_size = check_page_size(page_size).map_err(Error::corrupted)?;

        let minimal_file_size = page_size as u64 * 4;
        if options.read_only {
            let size = file
                .allocated_size()
                .map_err(io("Cannot get file allocated size"))?;
            if size < minimal_file_size {
                return Err(Error::FileTooSmall);
            }
        } else {
            file.allocate(minimal_file_size)
                .map_err(io("Cannot allocate 4 required pages"))?;
        }

        let mmap = unsafe {
            memmap2::MmapOptions::new()
                .offset(0)
                .map(&file)
                .map_err(io("Mmap failed"))?
        };

        let mut db = Self(Arc::new(DBInner {
            check_mode: options.checkmode,
            no_sync: false,
            no_grow_sync: options.no_grow_sync,
            max_batch_size: options.max_batch_size,
            max_batch_delay: options.max_batch_delay,
            autoremove: options.autoremove,
            alloc_size: 0,
            path,
            file: RwLock::new(BufWriter::new(file)),
            mmap_size: Mutex::new(0),
            mmap: RwLock::new(mmap),
            file_size: RwLock::new(0),
            page_size,
            opened: AtomicBool::new(true),
            rw_lock: Mutex::new(()),
            rw_tx: RwLock::new(None),
            txs: RwLock::new(Vec::new()),
            freelist: RwLock::new(FreeList::default()),
            stats: RwLock::from(Stats::default()),
            page_pool: Mutex::new(Vec::new()),
            batch: Mutex::new(None),
            read_only: options.read_only,
        }));

        if needs_initialization {
            db.init()?;
        }

        db.mmap(options.initial_mmap_size as u64)?;

        {
            let freelist_id = db.meta()?.freelist;
            db.mmap((freelist_id + 1) * db.0.page_size as u64)?;
            let freelist_page = db.page(freelist_id)?;
            db.0.freelist
                .try_write()
                .ok_or_else(|| Error::lock("freelist"))?
                .read(&freelist_page)?;
        }

        Ok(db)
    }

    pub(super) fn open<P: AsRef<Path>>(path: P, options: Options) -> Result<Self, Error> {
        let file = {
            let mut open_opts = OpenOptions::new();
            open_opts.read(true);
            if !options.read_only {
                open_opts.write(true).create(true);
            };
            open_opts.open(&path).map_err(|e| format!("{e}"))?
        };

        DB::open_file(file, path.as_ref(), options)
    }

    /// Returns meta info for given path
    pub fn get_meta(file: &mut File) -> Result<Meta, String> {
        let mut buf = [0u8; 12];
        file.seek(SeekFrom::Start(Page::header_size() as u64))
            .map_err(|e| format!("Can't seek file: {e}"))?;
        file.read_exact(&mut buf)
            .map_err(|e| format!("Can't read file to buffer: {e}"))?;
        let magic = u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
        if magic != MAGIC {
            return Err("Incorrect header".to_string());
        };
        let page_size =
            check_page_size(u32::from_ne_bytes([buf[8], buf[9], buf[10], buf[11]]) as usize)?;
        file.seek(SeekFrom::Start(0))
            .map_err(|e| format!("Can't seek file: {e}"))?;
        let mut metabuf = OwnedPage::new(page_size * 2);
        file.read_exact(metabuf.buf_mut())
            .map_err(|e| format!("Can't read file to buffer: {e}"))?;

        let metap0 = Page::from_buf(&metabuf.buf()[..page_size]).map_err(String::from)?;
        let metap1 = Page::from_buf(&metabuf.buf()[page_size..]).map_err(String::from)?;

        newest_valid_meta([metap0, metap1]).ok_or_else(|| "Invalid meta".to_string())
    }

    #[inline(always)]
    /// Returns database's defined page size
    pub fn page_size(&self) -> usize {
        self.0.page_size
    }

    pub(crate) fn remove_tx(&mut self, tx: &Tx) -> Result<Tx, Error> {
        if tx.writable() {
            let (freelist_free_n, freelist_pending_n, freelist_alloc) = {
                let freelist = self
                    .0
                    .freelist
                    .try_read()
                    .ok_or_else(|| Error::lock("freelist"))?;
                let freelist_free_n = freelist.free_count();
                let freelist_pending_n = freelist.pending_count();
                let freelist_alloc = freelist.size();
                (freelist_free_n, freelist_pending_n, freelist_alloc)
            };

            let tx = {
                let mut db_tx = self.0.rw_tx.write();
                if db_tx.is_none() {
                    return Err("No write transaction exists".into());
                };
                if !Arc::ptr_eq(&tx.0, &db_tx.as_ref().unwrap().0) {
                    return Err("Trying to remove unowned transaction".into());
                };
                db_tx.take().unwrap()
            };
            unsafe {
                self.0.rw_lock.raw().unlock();
            }

            let mut stats = self.0.stats.write();
            stats.free_page_n = freelist_free_n;
            stats.pending_page_n = freelist_pending_n;
            stats.free_alloc = (freelist_free_n + freelist_pending_n) * self.0.page_size;
            stats.freelist_in_use = freelist_alloc;
            stats.tx_stats += tx.stats();

            Ok(tx)
        } else {
            let mut txs = self
                .0
                .txs
                .try_write_for(Duration::from_secs(10))
                .ok_or_else(|| Error::lock("txs"))?;

            let index = txs.iter().position(|dbtx| Arc::ptr_eq(&tx.0, &dbtx.0));
            debug_assert!(index.is_some(), "trying to remove nonexistent tx");
            let index = index.unwrap();

            let tx = txs.remove(index);
            unsafe {
                self.0.mmap.raw().unlock_shared();
            }

            let mut stats = self.0.stats.write();
            stats.open_tx_n = txs.len();
            stats.tx_stats += tx.0.stats.lock().clone();

            Ok(tx)
        }
    }

    pub(super) fn init(&mut self) -> Result<(), Error> {
        let mut buf = OwnedPage::new(self.0.page_size * 4);
        for i in 0..=1 {
            let p = self.page_in_buffer(buf.buf_mut(), i)?;
            p.id = i;
            p.flags = Flags::META;
            let m = p.meta_mut()?;
            m.magic = MAGIC;
            m.version = VERSION;
            m.page_size = self.0.page_size as u32;
            m.freelist = 2;
            m.root = {
                let mut b = IBucket::new();
                b.root = 3;
                b
            };
            m.pgid = 4;
            m.txid = i;
            m.checksum = m.sum64();
        }

        let p = self.page_in_buffer(buf.buf_mut(), 2)?;
        p.id = 2;
        p.flags = Flags::FREELIST;

        let p = self.page_in_buffer(buf.buf_mut(), 3)?;
        p.id = 3;
        p.flags = Flags::LEAVES;

        self.write_at(0, std::io::Cursor::new(buf.buf()))?;
        self.sync()?;

        Ok(())
    }

    /// Closes database and removes it's data file
    pub fn remove(mut self) -> Result<(), Error> {
        if !self.0.opened.load(Ordering::Acquire) {
            return Ok(());
        }
        self.cleanup()?;

        if self.0.path.exists() {
            std::fs::remove_file(&self.0.path).map_err(io("Can't remove file"))?;
        }

        Ok(())
    }

    fn cleanup(&mut self) -> Result<(), Error> {
        if !self.0.opened.load(Ordering::Acquire) {
            return Ok(());
        }
        if self.0.check_mode.contains(CheckMode::CLOSE) {
            let strict = self.0.check_mode.contains(CheckMode::STRICT);
            let tx = self.begin_tx()?;
            if let Err(e) = tx.check_sync() {
                if strict {
                    return Err(e);
                } else {
                    println!("{e}");
                }
            }
        };
        // self.munmap()?;
        self.0.opened.store(false, Ordering::Release);

        let file = self.0.file.try_read().ok_or_else(|| Error::lock("file"))?;

        let file = file.get_ref();

        file.unlock().map_err(io("Can't unlock db file"))?;

        if self.0.autoremove && self.0.path.exists() {
            std::fs::remove_file(&self.0.path).map_err(io("Can't remove file"))?;
        }
        Ok(())
    }

    #[inline(always)]
    pub fn path(&self) -> &Path {
        &self.0.path
    }

    #[inline(always)]
    pub fn read_only(&self) -> bool {
        self.0.read_only
    }

    #[inline(always)]
    pub fn opened(&self) -> bool {
        self.0.opened.load(Ordering::Acquire)
    }

    /// Starts a new transaction.
    /// Multiple read-only transactions can be used concurrently but only one
    /// write transaction can be used at a time.
    ///
    /// Transactions should not be dependent on one another.
    ///
    /// If a long running read transaction (for example, a snapshot transaction) is
    /// needed, you might want to set DBBuilder.initial_mmap_size to a large enough value
    /// to avoid potential blocking of write transaction.
    pub fn begin_tx(&'a self) -> Result<TxGuard<'a>, Error> {
        if !self.opened() {
            return Err(Error::DatabaseClosed);
        };

        unsafe {
            self.0.mmap.raw().lock_shared();
        }

        let tx = match TxBuilder::new()
            .db(WeakDB::from(self))
            .writable(false)
            .check(self.0.check_mode.contains(CheckMode::READ))
            .build()
        {
            Ok(tx) => tx,
            Err(e) => {
                unsafe { self.0.mmap.raw().unlock_shared() };
                return Err(e);
            }
        };

        let mut txs = self.0.txs.write();
        txs.push(tx.clone());
        let txs_len = txs.len();
        drop(txs);

        {
            let mut stats = self.0.stats.write();
            stats.tx_n += 1;
            stats.open_tx_n = txs_len;
        }

        Ok(TxGuard {
            tx,
            db: std::marker::PhantomData,
        })
    }

    /// Starts a new writable transaction.
    /// Multiple read-only transactions can be used concurrently but only one
    /// write transaction can be used at a time.
    ///
    /// Transactions should not be dependent on one another.
    ///
    /// If a long running read transaction (for example, a snapshot transaction) is
    /// needed, you might want to set DBBuilder.initial_mmap_size to a large enough value
    /// to avoid potential blocking of write transaction.
    pub fn begin_rw_tx(&'a mut self) -> Result<RWTxGuard<'a>, Error> {
        if self.read_only() {
            return Err(Error::DatabaseReadonly);
        };
        if !self.opened() {
            return Err(Error::DatabaseClosed);
        };

        unsafe {
            self.0.rw_lock.raw().lock();
        };
        let tx = match self.begin_rw_tx_inner() {
            Ok(tx) => tx,
            Err(e) => {
                unsafe { self.0.rw_lock.raw().unlock() };
                return Err(e);
            }
        };

        Ok(RWTxGuard {
            tx,
            db: std::marker::PhantomData,
        })
    }

    fn begin_rw_tx_inner(&mut self) -> Result<Tx, Error> {
        let mut rw_tx = self.0.rw_tx.write();

        let mut minid = 0xFFFF_FFFF_FFFF_FFFF;
        for tx in self.0.txs.read().iter() {
            minid = minid.min(tx.id()?);
        }

        // Free any pages associated with closed read-only transactions.
        if minid > 0 {
            self.0
                .freelist
                .try_write()
                .ok_or_else(|| Error::lock("freelist"))?
                .release(minid - 1);
        }

        let tx = TxBuilder::new()
            .db(WeakDB::from(self))
            .writable(true)
            .check(self.0.check_mode.contains(CheckMode::WRITE))
            .build()?;
        *rw_tx = Some(tx.clone());
        Ok(tx)
    }

    /// shorthand for db.begin_rw_tx with additional guarantee for panic safery
    pub fn update<'b>(
        &mut self,
        mut handler: Box<dyn FnMut(&mut Tx) -> Result<(), String> + 'b>,
    ) -> Result<(), Error> {
        use std::panic::{AssertUnwindSafe, catch_unwind};

        let mut tx = scopeguard::guard(self.begin_rw_tx()?, |tx| {
            let db_exists = tx.db().is_ok();
            if db_exists {
                tx.rollback_inner().unwrap();
            };
        });

        let result = catch_unwind(AssertUnwindSafe(|| {
            tx.0.managed.store(true, Ordering::Release);
            let result = handler(&mut tx);
            tx.0.managed.store(false, Ordering::Release);
            result
        }));

        if result.is_err() {
            tx.rollback_inner()?;
            return Err("Panic while update".into());
        }

        let result = result.unwrap();
        if let Err(e) = result {
            tx.rollback()?;
            return Err(e.into());
        };

        tx.commit()
    }

    /// shorthand for db.begin_tx with additional guarantee for panic safery
    pub fn view<'b>(
        &self,
        handler: Box<dyn Fn(&Tx) -> Result<(), String> + 'b>,
    ) -> Result<(), Error> {
        use std::panic::{AssertUnwindSafe, catch_unwind};

        let tx = scopeguard::guard(self.begin_tx()?, |tx| {
            let db_exists = tx.db().is_ok();
            if db_exists {
                tx.rollback_inner().unwrap();
            };
        });

        let result = catch_unwind(AssertUnwindSafe(|| {
            tx.0.managed.store(true, Ordering::Release);
            let result = handler(&tx);
            tx.0.managed.store(false, Ordering::Release);
            result
        }));

        if result.is_err() {
            tx.rollback_inner()?;
            return Err("Panic while update".into());
        }

        let result = result.unwrap();
        if let Err(e) = result {
            tx.rollback()?;
            return Err(e.into());
        };

        tx.rollback()
    }

    /// Calls fn as part of a batch. It behaves similar to Update,
    /// except:
    ///
    /// 1. concurrent batch calls can be combined into a single
    ///    transaction.
    ///
    /// 2. the function passed to batch may be called multiple times,
    ///    regardless of whether it returns error or not.
    ///
    /// This means that Batch function side effects must be idempotent and
    /// take permanent effect only after a successful return is seen in
    /// caller.
    ///
    /// The maximum batch size and delay can be adjusted with DBBuilder.batch_size
    /// and DBBuilder.batch_delay, respectively.
    ///
    /// Batch is only useful when there are multiple threads calling it.
    /// While calling it multiple times from single thread just blocks
    /// thread for each single batch call
    pub fn batch(
        &mut self,
        handler: Box<dyn Fn(&mut Tx) -> Result<(), String> + Send>,
    ) -> Result<(), Error> {
        let weak_db = WeakDB::from(self);
        #[expect(clippy::arc_with_non_send_sync)]
        let handler = Arc::new(handler);
        let handler_clone = handler.clone();

        let (max_batch_size, max_batch_delay) = (self.0.max_batch_size, self.0.max_batch_delay);
        let err_receiver = {
            let (err_sender, err_receiver) = mpsc::channel();
            let mut call = Call::new(handler_clone, err_sender);
            let mut slot = self.0.batch.lock();
            loop {
                let batch = slot.get_or_insert_with(|| {
                    Batch::new(weak_db.clone(), max_batch_size, max_batch_delay)
                });
                match batch.push(call) {
                    Ok(()) => break,
                    Err(rejected) => {
                        call = rejected;
                        *slot = None;
                    }
                }
            }
            err_receiver
        };

        match err_receiver.recv() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) | Err(_) => self.update(Box::new(|tx| handler(tx))),
        }
    }

    pub(crate) fn write_at<T: Read>(&mut self, pos: u64, mut buf: T) -> Result<(), Error> {
        let mut file = self.0.file.write();
        file.seek(SeekFrom::Start(pos))
            .map_err(io("Can't seek to position"))?;
        std::io::copy(&mut buf, &mut *file).map_err(io("Can't write buffer"))?;
        Ok(())
    }

    pub(super) fn mmap(&mut self, mut min_size: u64) -> Result<(), Error> {
        let file = self
            .0
            .file
            .try_read_for(Duration::from_secs(60))
            .ok_or_else(|| Error::lock("file"))?;
        let mut mmap = self
            .0
            .mmap
            .try_write_for(Duration::from_secs(600))
            .ok_or_else(|| Error::lock("mmap"))?;

        let init_min_size = self.0.page_size as u64 * 4;

        min_size = u64::max(min_size, init_min_size);

        let mut size = self.mmap_size(min_size)?;

        if mmap.len() >= size as usize {
            return Ok(());
        }

        if self.0.read_only {
            let file_len = file
                .get_ref()
                .metadata()
                .map_err(io("Can't get file metadata"))?
                .len();
            size = u64::min(size, file_len);
        }

        let mut mmap_size = self.0.mmap_size.lock();

        if !self.read_only() {
            file.get_ref()
                .allocate(size)
                .map_err(|_| Error::ResizeFail)?;
        }

        // TODO: madvise
        let mut mmap_opts = memmap2::MmapOptions::new();
        let nmmap = unsafe {
            mmap_opts
                .offset(0)
                .len(size as usize)
                .map(file.get_ref())
                .map_err(io("Mmap failed"))?
        };
        *mmap_size = nmmap.len();
        *mmap = nmmap;

        drop(file);
        drop(mmap);
        drop(mmap_size);

        let check0 = self.page(0).and_then(|p| p.meta()?.validate());
        let check1 = self.page(1).and_then(|p| p.meta()?.validate());

        // Validate the meta pages. We only return an error if both meta pages fail
        // validation, since meta0 failing validation means that it wasn't saved
        // properly -- but we can recover using meta1. And vice-versa.
        if let Err(e) = check0
            && check1.is_err()
        {
            return Err(e);
        }

        Ok(())
    }

    pub(super) fn mmap_size(&self, mut size: u64) -> Result<u64, Error> {
        // Double the size from 32KB until 1GB.
        for i in 15..=30 {
            if size <= 1 << i {
                return Ok(1 << i);
            }
        }

        // Verify the requested size is not above the maximum allowed.
        if size > MAX_MAP_SIZE {
            return Err(Error::ResizeFail);
        };

        // If larger than 1GB then grow by 1GB at a time.
        let remainder = size % MAX_MMAP_STEP;
        if remainder > 0 {
            size += MAX_MMAP_STEP - remainder;
        };

        // Ensure that the mmap size is a multiple of the page size.
        // This should always be true since we're incrementing in MBs.
        let page_size = self.0.page_size as u64;
        if !size.is_multiple_of(page_size) {
            size = ((size / page_size) + 1) * page_size;
        };

        // If we've exceeded the max size then only grow up to the max size.
        if size > MAX_MAP_SIZE {
            size = MAX_MAP_SIZE
        };

        Ok(size)
    }

    pub(crate) fn sync(&mut self) -> Result<(), Error> {
        self.0.file.write().flush().map_err(|_| Error::SyncFail)
    }

    #[deprecated(since = "0.1.6", note = "use `try_stats` instead")]
    pub fn stats(&self) -> Stats {
        self.try_stats().unwrap()
    }

    /// Same as [`DB::stats`], but returns an error instead of panicking.
    pub fn try_stats(&self) -> Result<Stats, Error> {
        Ok(self
            .0
            .stats
            .try_read()
            .ok_or_else(|| Error::lock("stats"))?
            .clone())
    }

    #[deprecated(since = "0.1.6", note = "use `try_info` instead")]
    pub fn info(&self) -> Info {
        self.try_info().unwrap()
    }

    /// Same as [`DB::info`], but returns an error instead of panicking.
    pub fn try_info(&self) -> Result<Info, Error> {
        let ptr = self
            .0
            .mmap
            .try_read()
            .ok_or_else(|| Error::lock("mmap"))?
            .as_ptr();
        Ok(Info {
            data: ptr,
            page_size: self.0.page_size as i64,
        })
    }

    /// Retrieves page from mmap
    pub(crate) fn page(&self, id: PGID) -> Result<MappedRwLockReadGuard<'_, Page>, Error> {
        self.mapped_page(id, true)
    }

    pub(crate) fn page_header(&self, id: PGID) -> Result<MappedRwLockReadGuard<'_, Page>, Error> {
        self.mapped_page(id, false)
    }

    fn mapped_page(
        &self,
        id: PGID,
        with_data: bool,
    ) -> Result<MappedRwLockReadGuard<'_, Page>, Error> {
        let out_of_bounds = || Error::corrupted(format!("Page {id}: out of bounds"));
        let mmap = self.0.mmap.read_recursive();
        let page_size = self.0.page_size as u64;
        let pos = id
            .checked_mul(page_size)
            .and_then(|pos| usize::try_from(pos).ok())
            .ok_or_else(out_of_bounds)?;
        let len = {
            let rest = mmap.get(pos..).ok_or_else(out_of_bounds)?;
            let header = Page::from_buf(rest).map_err(|_| out_of_bounds())?;
            let len = if with_data {
                usize::try_from((u64::from(header.overflow) + 1) * page_size)
                    .map_err(|_| out_of_bounds())?
            } else {
                Page::header_size()
            };
            if len > rest.len() {
                return Err(out_of_bounds());
            }
            len
        };
        RwLockReadGuard::try_map(mmap, |mmap| {
            mmap.get(pos..pos + len)
                .and_then(|buf| Page::from_buf(buf).ok())
        })
        .map_err(|_| out_of_bounds())
    }

    fn page_in_buffer<'b>(&'a self, buf: &'b mut [u8], id: PGID) -> Result<&'b mut Page, Error> {
        let page_size = self.0.page_size;
        let pos = id as usize * page_size;
        let buf = buf
            .get_mut(pos..pos + page_size)
            .ok_or("Page buffer too small")?;
        Page::from_buf_mut(buf)
    }

    pub fn meta(&self) -> Result<Meta, Error> {
        let pages = [self.page(0), self.page(1)];
        newest_valid_meta(pages.iter().filter_map(|page| page.as_deref().ok())).ok_or(Error::NoMeta)
    }

    pub(crate) fn allocate(&mut self, count: u64, tx: &mut Tx) -> Result<OwnedPage, Error> {
        let mut p = if count == 1 {
            let mut pagepool = self.0.page_pool.lock();
            if let Some(p) = pagepool.pop() {
                p
            } else {
                OwnedPage::new(self.0.page_size)
            }
        } else {
            OwnedPage::new(self.0.page_size * count as usize)
        };
        p.overflow = count as u32 - 1;

        {
            let freelist_pid = self.0.freelist.write().allocate(count)?;
            if freelist_pid != 0 {
                p.id = freelist_pid;
                return Ok(p);
            };
        }

        p.id = tx.pgid()?;

        // Resize mmap() if we're at the end.
        let minsz = (p.id + 1 + count) * self.0.page_size as u64;
        let mmap_size = *self.0.mmap_size.lock() as u64;
        if minsz >= mmap_size {
            self.mmap(minsz)?;
        };

        // Move the page id high water mark.
        tx.set_pgid(tx.pgid()? + count)?;

        Ok(p)
    }

    pub(crate) fn grow(&mut self, mut size: u64) -> Result<(), Error> {
        if self.0.read_only {
            return Err(Error::DatabaseReadonly);
        };

        let file = self.0.file.try_write().ok_or_else(|| Error::lock("file"))?;

        if file.get_ref().metadata()?.len() >= size {
            return Ok(());
        }

        // If the data is smaller than the alloc size then only allocate what's needed.
        // Once it goes over the allocation size then allocate in chunks.
        {
            let mmapsize = self
                .0
                .mmap
                .try_read()
                .ok_or_else(|| Error::lock("mmap"))?
                .as_ref()
                .len() as u64;
            if mmapsize < self.0.alloc_size {
                size = mmapsize;
            } else {
                size += self.0.alloc_size;
            };
        }

        file.get_ref()
            .allocate(size)
            .map_err(|_e| Error::ResizeFail)?;

        if !self.0.no_grow_sync {
            file.get_ref().sync_all().map_err(|_| Error::SyncFail)?;
        }

        *self.0.file_size.write() = file
            .get_ref()
            .metadata()
            .map_err(io("Can't get metadata file"))?
            .len();
        Ok(())
    }
}

impl Drop for DB {
    fn drop(&mut self) {
        if Arc::strong_count(&self.0) > 1 {
            return;
        };
        self.cleanup().unwrap();
    }
}

#[derive(Clone)]
pub(crate) struct WeakDB(Weak<DBInner>);

impl WeakDB {
    pub(crate) fn new() -> Self {
        Self(Weak::new())
    }

    pub(crate) fn upgrade(&self) -> Option<DB> {
        self.0.upgrade().map(DB)
    }

    pub(crate) fn from(db: &DB) -> Self {
        Self(Arc::downgrade(&db.0))
    }

    pub(crate) fn as_ptr(&self) -> *const () {
        self.0.as_ptr().cast()
    }
}

fn newest_valid_meta<'a>(pages: impl IntoIterator<Item = &'a Page>) -> Option<Meta> {
    pages
        .into_iter()
        .filter_map(|page| page.meta().ok().filter(|meta| meta.validate().is_ok()))
        .reduce(|newest, meta| {
            if meta.txid > newest.txid {
                meta
            } else {
                newest
            }
        })
        .cloned()
}

fn check_page_size(page_size: usize) -> Result<usize, String> {
    if page_size < Page::header_size() + Meta::SIZE || !page_size.is_multiple_of(align_of::<PGID>())
    {
        return Err(format!("Invalid page size: {page_size}"));
    }
    Ok(page_size)
}
