//! `admin.key`: the whole key or nothing, never over another (D60, SRV-7).

use std::fs::{File, Permissions};
use std::io::{self, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use ironflowx_core::Secret;

use crate::logging::public_text;

pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("admin.key")
}

/// Whether the name is taken, as by a symbolic link to nothing.
pub fn exists(data_dir: &Path) -> bool {
    std::fs::symlink_metadata(path(data_dir)).is_ok()
}

/// Writes `key` and a newline to `admin.key`, which must not exist. A crash leaves the whole key
/// there or no `admin.key`, and at most a temporary file beside it.
pub fn write(data_dir: &Path, key: &Secret) -> io::Result<()> {
    let mut temporary = tempfile::Builder::new()
        .prefix("admin.key.")
        .suffix(".tmp")
        .permissions(Permissions::from_mode(0o400))
        .tempfile_in(data_dir)?;
    // The process umask may have taken the owner's bit too.
    temporary
        .as_file()
        .set_permissions(Permissions::from_mode(0o400))?;
    temporary.write_all(key.expose().as_bytes())?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    // Not a rename, which would replace an `admin.key`.
    std::fs::hard_link(temporary.path(), path(data_dir))?;
    temporary.close()?;
    File::open(data_dir)?.sync_all()
}

/// Warns about names left by another writer or a crash. They may hold the only copy of a key.
pub fn warn_temporaries(data_dir: &Path) {
    let entries = match std::fs::read_dir(data_dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!(
                path = %public_text(&data_dir.to_string_lossy()),
                error = %public_text(&e.to_string()),
                "cannot look for temporary admin keys"
            );
            return;
        }
    };
    for entry in entries {
        match entry {
            Ok(entry) => {
                let name = entry.file_name();
                if name.as_encoded_bytes().starts_with(b"admin.key.")
                    && name.as_encoded_bytes().ends_with(b".tmp")
                {
                    tracing::warn!(
                        path = %public_text(&entry.path().to_string_lossy()),
                        "temporary admin key is kept"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(error = %public_text(&e.to_string()), "cannot read a directory entry")
            }
        }
    }
}

/// Exercises the write, sync, link and unlink in the same directory as the real key file.
pub fn probe(data_dir: &Path) -> io::Result<()> {
    probe_with_link(data_dir, |source, link| std::fs::hard_link(source, link))
}

fn probe_with_link(
    data_dir: &Path,
    link_file: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let mut temporary = tempfile::Builder::new()
        .prefix(".admin-key-probe.")
        .permissions(Permissions::from_mode(0o400))
        .tempfile_in(data_dir)?;
    let mut link = temporary.path().as_os_str().to_owned();
    link.push(".link");
    let link = PathBuf::from(link);

    let test = (|| {
        temporary
            .as_file()
            .set_permissions(Permissions::from_mode(0o400))?;
        // The final file holds an 84-byte key and a newline.
        temporary.write_all(&[0; 85])?;
        temporary.as_file().sync_all()?;
        link_file(temporary.path(), &link)
    })();

    // Only a successful link gives us a second name to remove. Always try the first one too.
    let unlink = test.as_ref().ok().and_then(|()| {
        std::fs::remove_file(&link)
            .err()
            .map(|e| format!("cannot remove {}: {e}", link.display()))
    });
    let close = temporary.close().err().map(|e| e.to_string());
    let cleanup: Vec<_> = [unlink, close].into_iter().flatten().collect();
    if !cleanup.is_empty() {
        let cleanup = cleanup.join("; ");
        return Err(match test {
            Ok(()) => io::Error::other(cleanup),
            Err(e) => io::Error::other(format!("{e}; cleanup: {cleanup}")),
        });
    }
    test?;
    File::open(data_dir)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_probe_keeps_an_entry_it_could_not_link_over() {
        let dir = tempfile::tempdir().unwrap();
        let mut collision = PathBuf::new();
        let error = probe_with_link(dir.path(), |source, link| {
            collision = link.to_owned();
            std::fs::write(link, b"another writer")?;
            std::fs::hard_link(source, link)
        })
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&collision).unwrap(), b"another writer");
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, [collision.file_name().unwrap()]);
    }

    #[test]
    fn the_probe_reports_both_paths_if_the_os_refuses_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let mut paths = None;
        let result = probe_with_link(dir.path(), |source, link| {
            std::fs::hard_link(source, link)?;
            paths = Some([source.to_owned(), link.to_owned()]);
            std::fs::set_permissions(dir.path(), Permissions::from_mode(0o500))
        });
        std::fs::set_permissions(dir.path(), Permissions::from_mode(0o700)).unwrap();

        let error = result.unwrap_err().to_string();
        let [source, link] = paths.unwrap();
        assert!(error.contains(&format!("at path {source:?}")), "{error}");
        assert!(
            error.contains(&format!("cannot remove {}:", link.display())),
            "{error}"
        );
        for path in [source, link] {
            assert_eq!(std::fs::read(path).unwrap(), [0; 85]);
        }
        assert!(!dir.path().join("control.db").exists());
        assert!(!path(dir.path()).exists());
    }
}
