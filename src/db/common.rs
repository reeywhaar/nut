use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use crate::errors::Error;
use crate::tx::Tx;

use super::WeakDB;

#[expect(clippy::type_complexity)]
pub(super) struct Call {
    h: Arc<Box<dyn Fn(&mut Tx) -> Result<(), String> + Send>>,
    err: mpsc::Sender<Result<(), String>>,
}

impl Call {
    #[expect(clippy::type_complexity)]
    pub(super) fn new(
        func: Arc<Box<dyn Fn(&mut Tx) -> Result<(), String> + Send>>,
        err_ch: mpsc::Sender<Result<(), String>>,
    ) -> Self {
        Self {
            h: func,
            err: err_ch,
        }
    }
}

struct BatchInner {
    db: WeakDB,
    calls_len: usize,
    ran: std::sync::atomic::AtomicBool,
    pub(super) calls: Mutex<Vec<Call>>,
}

#[derive(Clone)]
pub(crate) struct Batch(Arc<BatchInner>);

unsafe impl Send for Batch {}
unsafe impl Sync for Batch {}

impl Batch {
    pub(super) fn new(db: WeakDB, calls_len: usize, delay: Duration) -> Self {
        #[expect(clippy::arc_with_non_send_sync)]
        let batch = Batch(Arc::new(BatchInner {
            db,
            calls_len,
            ran: AtomicBool::new(false),
            calls: Mutex::new(Vec::new()),
        }));

        let mut bc = batch.clone();
        thread::spawn(move || {
            thread::sleep(delay);
            bc.trigger();
        });

        batch
    }

    pub(super) fn push(&mut self, call: Call) -> Result<(), Call> {
        let calls_len = {
            let mut calls = self.0.calls.lock();
            if self.0.ran.load(Ordering::Acquire) {
                return Err(call);
            };
            calls.push(call);
            calls.len()
        };
        if self.0.calls_len != 0 && calls_len >= self.0.calls_len {
            let mut bc = self.clone();
            thread::spawn(move || {
                bc.trigger();
            });
        };
        Ok(())
    }

    /// trigger runs the batch if it hasn't already been run.
    pub(super) fn trigger(&mut self) {
        let calls = self.0.calls.lock();
        let already_ran = self.0.ran.swap(true, Ordering::AcqRel);
        drop(calls);
        if !already_ran {
            self.run();
        }
    }

    /// run performs the transactions in the batch and communicates results
    /// back to DB.Batch.
    fn run(&mut self) {
        let Some(mut db) = self.0.db.upgrade() else {
            for call in self.0.calls.lock().drain(..) {
                call.err
                    .send(Err(Error::DatabaseGone.to_string()))
                    .expect("Batch caller dropped");
            }
            return;
        };
        {
            let mut slot = db.0.batch.lock();
            if slot.as_ref().is_some_and(|b| Arc::ptr_eq(&b.0, &self.0)) {
                *slot = None;
            }
        }

        let mut calls = self.0.calls.lock();

        while !calls.is_empty() {
            let mut last_call_id = 0;
            if let Err(e) = db.update(Box::new(|tx| {
                for (index, call) in calls.iter().enumerate() {
                    last_call_id = index;
                    (call.h)(tx)?
                }
                Ok(())
            })) {
                let failed_call = calls.remove(last_call_id);
                failed_call
                    .err
                    .send(Err(format!("{e}")))
                    .expect("Batch caller dropped");
                continue;
            }
            {
                for call in &*calls {
                    call.err.send(Ok(())).expect("Batch caller dropped");
                }
                break;
            }
        }
    }
}
