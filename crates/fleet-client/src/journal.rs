//! Private, cross-process serialized SDK journals.
//! File locking coordinates Fleet SDK writers, not malicious same-user code.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(super) fn path(override_name: &str, name: &str) -> io::Result<PathBuf> {
    if let Some(value) = std::env::var_os(override_name) {
        let path = PathBuf::from(value);
        if !path.is_absolute() {
            return Err(invalid("journal override must be absolute"));
        }
        return Ok(path);
    }
    #[cfg(windows)]
    let root = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(unix)]
    let root = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")));
    let root = root
        .filter(|path| path.is_absolute())
        .ok_or_else(|| invalid("no absolute durable journal root"))?;
    Ok(root.join("gentle-fleet").join(name))
}

struct Journal {
    file: File,
    #[cfg(windows)]
    _directory: fleet_protocol::transport::DataDirectory,
}

impl Journal {
    fn open(path: &Path, writing: bool) -> io::Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| invalid("journal needs a parent directory"))?;
        if !path.is_absolute()
            || path.file_name().is_none()
            || path
                .components()
                .any(|part| part == std::path::Component::ParentDir)
        {
            return Err(invalid(
                "journal requires an absolute path without parent traversal",
            ));
        }
        #[cfg(windows)]
        let directory = {
            let directory = fleet_protocol::transport::DataDirectory::open(parent)?;
            directory.validate_private_file(path)?;
            directory
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
            if writing {
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(parent)?;
            }
            let metadata = std::fs::symlink_metadata(parent)?;
            if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
                return Err(invalid("journal directory is not private"));
            }
            match std::fs::symlink_metadata(path) {
                Ok(metadata) => {
                    if !metadata.is_file()
                        || metadata.nlink() != 1
                        || metadata.permissions().mode() & 0o077 != 0
                    {
                        return Err(invalid("journal file is not private and regular"));
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        let mut options = OpenOptions::new();
        options.read(true);
        if writing {
            // Not append-only: Windows requires a read/write handle for locks.
            // Seek and write under the same exclusive lock.
            options.write(true).create(true).truncate(false);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path)?;
        #[cfg(windows)]
        directory.validate_private_file(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let metadata = file.metadata()?;
            if !metadata.is_file()
                || metadata.nlink() != 1
                || metadata.permissions().mode() & 0o077 != 0
            {
                return Err(invalid("journal handle is not private and regular"));
            }
        }
        if writing {
            file.lock()?;
        } else {
            file.lock_shared()?;
        }
        Ok(Self {
            file,
            #[cfg(windows)]
            _directory: directory,
        })
    }

    fn verify_tail(&mut self) -> io::Result<()> {
        if self.file.metadata()?.len() != 0 {
            self.file.seek(SeekFrom::End(-1))?;
            let mut last = [0];
            self.file.read_exact(&mut last)?;
            if last[0] != b'\n' {
                // Never concatenate a new record to a torn record or erase
                // an uncertain operation. Operator recovery is explicit.
                return Err(invalid(
                    "incomplete final journal record; recovery required",
                ));
            }
        }
        Ok(())
    }
}

pub(super) fn append(path: &Path, line: &[u8]) -> io::Result<()> {
    if !line.ends_with(b"\n") || line[..line.len() - 1].contains(&b'\n') {
        return Err(invalid("journal requires one complete JSONL record"));
    }
    let mut journal = Journal::open(path, true)?;
    journal.verify_tail()?;
    journal.file.seek(SeekFrom::End(0))?;
    journal.file.write_all(line)?;
    journal.file.sync_data()
}

pub(super) fn read(path: &Path) -> io::Result<String> {
    let mut journal = match Journal::open(path, false) {
        Ok(journal) => journal,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(error),
    };
    journal.verify_tail()?;
    journal.file.seek(SeekFrom::Start(0))?;
    let mut content = String::new();
    journal.file.read_to_string(&mut content)?;
    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("gf-sdk-journal-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }
        fn path(&self) -> PathBuf {
            self.0.join("data").join("journal.jsonl")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn concurrent_journal_writers_preserve_complete_records() {
        let fixture = Fixture::new();
        let path = fixture.path();
        let threads: Vec<_> = (0..4)
            .map(|writer| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for index in 0..32 {
                        let line = format!("{{\"writer\":{writer},\"index\":{index}}}\n");
                        append(&path, line.as_bytes()).unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let content = read(&path).unwrap();
        let records: std::collections::HashSet<_> = content
            .lines()
            .map(|line| {
                let record: serde_json::Value = serde_json::from_str(line).unwrap();
                (
                    record["writer"].as_u64().unwrap(),
                    record["index"].as_u64().unwrap(),
                )
            })
            .collect();
        assert_eq!(content.lines().count(), 128);
        assert_eq!(records.len(), 128);
    }

    #[test]
    fn incomplete_tail_is_not_overwritten_or_joined_to_another_record() {
        let fixture = Fixture::new();
        let path = fixture.path();
        append(&path, b"{\"first\":1}\n").unwrap();
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"torn\":").unwrap();
        file.sync_data().unwrap();
        drop(file);
        let before = std::fs::read(&path).unwrap();
        assert!(read(&path)
            .unwrap_err()
            .to_string()
            .contains("incomplete final"));
        assert!(append(&path, b"{\"next\":2}\n").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn cursor_reopen_is_host_scoped_and_rejects_corrupt_middle_records() {
        let fixture = Fixture::new();
        let path = fixture.path();
        for record in [
            b"{\"host\":\"local\",\"session_id\":\"s\",\"seq\":7}\n".as_slice(),
            b"{\"host\":\"ssh:other\",\"session_id\":\"s\",\"seq\":99}\n",
            b"{\"host\":\"local\",\"session_id\":\"s\",\"seq\":3}\n",
        ] {
            append(&path, record).unwrap();
        }
        assert_eq!(
            crate::load_cursor_at(&path, &crate::Host::Local, "s").unwrap(),
            7
        );
        assert_eq!(
            crate::load_cursor_at(&path, &crate::Host::Ssh("other".into()), "s").unwrap(),
            99
        );
        append(&path, b"{\"seq\":\"invalid\"}\n").unwrap();
        append(
            &path,
            b"{\"host\":\"local\",\"session_id\":\"s\",\"seq\":8}\n",
        )
        .unwrap();
        assert!(matches!(
            crate::load_cursor_at(&path, &crate::Host::Local, "s"),
            Err(crate::ClientError::Protocol(_))
        ));
    }

    #[test]
    fn hardlinked_journal_is_rejected_without_mutation() {
        let fixture = Fixture::new();
        let path = fixture.path();
        append(&path, b"{}\n").unwrap();
        let alias = path.with_file_name("alias");
        std::fs::hard_link(&path, &alias).unwrap();
        assert!(read(&path).is_err());
        assert!(append(&path, b"{}\n").is_err());
        assert_eq!(std::fs::read(&alias).unwrap(), b"{}\n");
    }
}
