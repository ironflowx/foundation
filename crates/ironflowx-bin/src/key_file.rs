//! `admin.key`: the whole key or nothing, never over another (D60, SRV-7).

use std::fs::{File, Permissions};
use std::io::{self, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use ironflowx_core::Secret;

pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("admin.key")
}

/// Writes `key` and a newline to `admin.key`, which must not exist. A crash leaves the whole key
/// there or no `admin.key`, and at most a temporary file beside it.
pub fn write(data_dir: &Path, key: &Secret) -> io::Result<()> {
    let mut temporary = tempfile::Builder::new()
        .prefix("admin.key.")
        .suffix(".tmp")
        .permissions(Permissions::from_mode(0o400))
        .tempfile_in(data_dir)?;
    temporary.write_all(key.expose().as_bytes())?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    // Not a rename, which would replace an `admin.key`.
    std::fs::hard_link(temporary.path(), path(data_dir))?;
    temporary.close()?;
    File::open(data_dir)?.sync_all()
}
