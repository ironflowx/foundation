//! How a test knows that a writer it cannot stop has come to its write: SQLite's statement trace.
//! `ironflowx-operations`' tests include this file by path.

use std::ffi::{CStr, c_char, c_int, c_uint, c_void};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once};

use rusqlite::ffi;

/// The file of each `BEGIN IMMEDIATE` this process began on a traced connection, whether or not it
/// has the lock yet.
static BEGUN: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// SQLite calls it as a statement starts, before the statement takes a lock.
unsafe extern "C" fn on_statement(
    _: c_uint,
    _: *mut c_void,
    statement: *mut c_void,
    sql: *mut c_void,
) -> c_int {
    // SAFETY: for `SQLITE_TRACE_STMT`, the running statement and its text; its connection's file
    // name is a C string too.
    let (sql, file) = unsafe {
        let db = ffi::sqlite3_db_handle(statement.cast());
        let file = ffi::sqlite3_db_filename(db, c"main".as_ptr());
        (CStr::from_ptr(sql.cast()), CStr::from_ptr(file))
    };
    if sql == c"BEGIN IMMEDIATE" {
        let file = PathBuf::from(file.to_str().unwrap());
        BEGUN.lock().unwrap().push(file);
    }
    0
}

/// SQLite calls it for each connection it opens.
unsafe extern "C" fn on_connection(
    db: *mut ffi::sqlite3,
    _: *mut *mut c_char,
    _: *const ffi::sqlite3_api_routines,
) -> c_int {
    let mask = ffi::SQLITE_TRACE_STMT;
    // SAFETY: `db` is the connection being opened; the callback uses no context.
    unsafe { ffi::sqlite3_trace_v2(db, mask, Some(on_statement), std::ptr::null_mut()) }
}

/// Traces each connection opened from here on.
pub fn trace() {
    static TRACED: Once = Once::new();
    TRACED.call_once(|| {
        // SAFETY: `on_connection` has the signature SQLite calls.
        let code = unsafe { ffi::sqlite3_auto_extension(Some(on_connection)) };
        assert_eq!(code, ffi::SQLITE_OK);
    });
}

/// How many `BEGIN IMMEDIATE`s began on `control_db`.
pub fn begun(control_db: &Path) -> usize {
    let file = control_db.canonicalize().unwrap();
    BEGUN.lock().unwrap().iter().filter(|f| **f == file).count()
}
