use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::bucket::Bucket;
use crate::db::WeakDB;
use crate::errors::Error;
use crate::meta::Meta;

use super::{Tx, TxInner, WeakTx};

pub(crate) struct TxBuilder {
    db: WeakDB,
    writable: bool,
    check: bool,
}

impl TxBuilder {
    pub(crate) fn new() -> Self {
        Self {
            db: WeakDB::new(),
            writable: false,
            check: false,
        }
    }

    pub(crate) fn db(mut self, value: WeakDB) -> Self {
        self.db = value;
        self
    }

    pub(crate) fn writable(mut self, value: bool) -> Self {
        self.writable = value;
        self
    }

    pub(crate) fn check(mut self, value: bool) -> Self {
        self.check = value;
        self
    }

    pub fn build(self) -> Result<Tx, Error> {
        let mut meta = match self.db.upgrade() {
            None => Meta::default(),
            Some(db) => db.meta()?,
        };
        if self.writable {
            meta.txid += 1;
        };

        #[expect(clippy::arc_with_non_send_sync)]
        let tx = Tx(Arc::new(TxInner {
            writable: self.writable,
            managed: AtomicBool::new(false),
            check: AtomicBool::new(self.check),
            closed: AtomicBool::new(self.db.upgrade().is_none()),
            db: RwLock::new(self.db),
            meta: RwLock::new(meta),
            root: RwLock::new(Bucket::new(WeakTx::new())),
            pages: RwLock::new(HashMap::new()),
            stats: Mutex::new(Default::default()),
            commit_handlers: Mutex::new(Vec::new()),
            write_flag: 0,
        }));

        {
            let mut bucket = tx.0.root.write();
            bucket.tx = WeakTx::from(&tx);
            bucket.bucket =
                tx.0.meta
                    .try_read()
                    .ok_or_else(|| Error::lock("meta"))?
                    .root
                    .clone();
        }

        Ok(tx)
    }
}
