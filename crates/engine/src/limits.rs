//! What the app can keep a caller from: reading and writing files, and closing drawings. The
//! app serves the AI assistant's requests under [`deny_files`] (Options ▸ AI) and
//! [`deny_closing`], and every command checks [`files`] before it opens, reads or writes a file,
//! and [`closing`] before it closes a drawing, whatever the parameter, typed answer, script,
//! LISP or alias that led there. A limit holds on the thread that set it: the session runs on one.

use std::cell::Cell;
use std::thread::LocalKey;

use crate::{EngineError, Result};

type Limit = Cell<Option<&'static str>>;

thread_local! {
    static FILES: Limit = const { Cell::new(None) };
    static CLOSING: Limit = const { Cell::new(None) };
}

/// Reading and writing files on this thread fail with `why` until the guard is dropped.
#[must_use]
pub fn deny_files(why: &'static str) -> Lifted {
    Lifted::set(&FILES, why)
}

/// Closing drawings on this thread fails with `why` until the guard is dropped.
#[must_use]
pub fn deny_closing(why: &'static str) -> Lifted {
    Lifted::set(&CLOSING, why)
}

/// Restores the limit that was in force before (also when a command panics).
pub struct Lifted {
    limit: &'static LocalKey<Limit>,
    before: Option<&'static str>,
}

impl Lifted {
    fn set(limit: &'static LocalKey<Limit>, why: &'static str) -> Self {
        Lifted { limit, before: limit.replace(Some(why)) }
    }
}

impl Drop for Lifted {
    fn drop(&mut self) {
        self.limit.set(self.before);
    }
}

/// Why file access is denied on this thread, if it is.
pub fn files_denied() -> Option<&'static str> {
    FILES.get()
}

/// `Err` for `cmd` while file access is denied.
pub fn files(cmd: &str) -> Result<()> {
    check(&FILES, cmd)
}

/// `Err` for `cmd` while closing drawings is denied.
pub fn closing(cmd: &str) -> Result<()> {
    check(&CLOSING, cmd)
}

fn check(limit: &'static LocalKey<Limit>, cmd: &str) -> Result<()> {
    match limit.get() {
        Some(why) => Err(EngineError::Disabled(cmd.into(), why.into())),
        None => Ok(()),
    }
}
