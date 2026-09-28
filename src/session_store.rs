//! Persistence for the opaque Brickwave app session.
//!
//! Windows uses DPAPI, so the proof can only be decrypted by the same user.
//! StockOS stores the opaque, expiring Worker proof with an atomic same-folder
//! rename; FAT cannot enforce Unix permissions. SoundCloud access and refresh
//! tokens never reach this process and remain in the Cloudflare Durable Object.

use crate::backend::UserSession;

#[derive(Debug)]
pub enum SessionStoreError {
    Io,
    InvalidData,
    PlatformUnavailable,
}

#[cfg(windows)]
mod platform {
    use super::{SessionStoreError, UserSession};
    use serde::{Deserialize, Serialize};
    use std::ffi::c_void;
    use std::fs;
    use std::path::PathBuf;
    use std::ptr;
    use std::time::{SystemTime, UNIX_EPOCH};

    const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;
    const STORE_VERSION: u8 = 1;

    #[repr(C)]
    struct DataBlob {
        cb_data: u32,
        pb_data: *mut u8,
    }

    #[link(name = "crypt32")]
    unsafe extern "system" {
        fn CryptProtectData(
            data_in: *const DataBlob,
            description: *const u16,
            optional_entropy: *const DataBlob,
            reserved: *mut c_void,
            prompt: *mut c_void,
            flags: u32,
            data_out: *mut DataBlob,
        ) -> i32;
        fn CryptUnprotectData(
            data_in: *const DataBlob,
            description: *mut *mut u16,
            optional_entropy: *const DataBlob,
            reserved: *mut c_void,
            prompt: *mut c_void,
            flags: u32,
            data_out: *mut DataBlob,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }

    #[derive(Serialize, Deserialize)]
    struct StoredSession {
        version: u8,
        session: UserSession,
    }

    fn path() -> Result<PathBuf, SessionStoreError> {
        let root = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .ok_or(SessionStoreError::PlatformUnavailable)?;
        Ok(root.join("Brickwave").join("session.dat"))
    }

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    fn crypt(input: &[u8], protect: bool) -> Result<Vec<u8>, SessionStoreError> {
        let input_len = u32::try_from(input.len()).map_err(|_| SessionStoreError::InvalidData)?;
        let input_blob = DataBlob {
            cb_data: input_len,
            pb_data: input.as_ptr() as *mut u8,
        };
        let mut output_blob = DataBlob {
            cb_data: 0,
            pb_data: ptr::null_mut(),
        };
        let ok = unsafe {
            if protect {
                CryptProtectData(
                    &input_blob,
                    ptr::null(),
                    ptr::null(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output_blob,
                )
            } else {
                CryptUnprotectData(
                    &input_blob,
                    ptr::null_mut(),
                    ptr::null(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output_blob,
                )
            }
        };
        if ok == 0 || output_blob.pb_data.is_null() {
            return Err(SessionStoreError::InvalidData);
        }
        let output = unsafe {
            std::slice::from_raw_parts(output_blob.pb_data, output_blob.cb_data as usize).to_vec()
        };
        unsafe {
            LocalFree(output_blob.pb_data.cast());
        }
        Ok(output)
    }

    pub fn load() -> Result<Option<UserSession>, SessionStoreError> {
        let path = path()?;
        let encrypted = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(SessionStoreError::Io),
        };
        let decoded = crypt(&encrypted, false)?;
        let stored: StoredSession =
            serde_json::from_slice(&decoded).map_err(|_| SessionStoreError::InvalidData)?;
        if stored.version != STORE_VERSION || stored.session.expires_at_ms <= now_ms() {
            let _ = fs::remove_file(path);
            return Ok(None);
        }
        Ok(Some(stored.session))
    }

    pub fn save(session: &UserSession) -> Result<(), SessionStoreError> {
        let path = path()?;
        let parent = path.parent().ok_or(SessionStoreError::Io)?;
        fs::create_dir_all(parent).map_err(|_| SessionStoreError::Io)?;
        let serialized = serde_json::to_vec(&StoredSession {
            version: STORE_VERSION,
            session: session.clone(),
        })
        .map_err(|_| SessionStoreError::InvalidData)?;
        let encrypted = crypt(&serialized, true)?;
        fs::write(path, encrypted).map_err(|_| SessionStoreError::Io)
    }

    pub fn clear() -> Result<(), SessionStoreError> {
        let path = path()?;
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(SessionStoreError::Io),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::crypt;

        #[test]
        fn dpapi_round_trip_does_not_leave_plaintext() {
            let plaintext = b"opaque-worker-session-test";
            let encrypted = crypt(plaintext, true).unwrap();
            assert_ne!(encrypted, plaintext);
            assert_eq!(crypt(&encrypted, false).unwrap(), plaintext);
        }
    }
}

#[cfg(all(not(windows), unix))]
mod platform {
    use super::{SessionStoreError, UserSession};
    use serde::{Deserialize, Serialize};
    use std::fs::{self, File, OpenOptions};
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    const STORE_VERSION: u8 = 1;
    const MAX_STORE_BYTES: u64 = 16 * 1024;
    const STORE_FILE: &str = "session.json";
    const TEMP_FILE: &str = "session.json.tmp";

    #[derive(Serialize, Deserialize)]
    struct StoredSession {
        version: u8,
        session: UserSession,
    }

    fn data_dir() -> Result<PathBuf, SessionStoreError> {
        if let Some(configured) = std::env::var_os("BRICKWAVE_DATA_DIR") {
            if !configured.is_empty() {
                return Ok(PathBuf::from(configured));
            }
        }
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            if !xdg.is_empty() {
                return Ok(PathBuf::from(xdg).join("brickwave"));
            }
        }
        std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .map(|home| home.join(".local").join("share").join("brickwave"))
            .ok_or(SessionStoreError::PlatformUnavailable)
    }

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    fn valid_opaque_id(value: &str) -> bool {
        (20..=128).contains(&value.len())
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    }

    fn valid_session(session: &UserSession, now: u64) -> bool {
        valid_opaque_id(&session.session_id)
            && valid_opaque_id(&session.session_secret)
            && session.expires_at_ms > now
    }

    fn remove_if_present(path: &Path) -> Result<(), SessionStoreError> {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(SessionStoreError::Io),
        }
    }

    fn load_at(path: &Path, now: u64) -> Result<Option<UserSession>, SessionStoreError> {
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(SessionStoreError::Io),
        };
        if !metadata.is_file() || metadata.len() > MAX_STORE_BYTES {
            return Err(SessionStoreError::InvalidData);
        }
        let encoded = fs::read(path).map_err(|_| SessionStoreError::Io)?;
        if encoded.len() as u64 > MAX_STORE_BYTES {
            return Err(SessionStoreError::InvalidData);
        }
        let stored: StoredSession =
            serde_json::from_slice(&encoded).map_err(|_| SessionStoreError::InvalidData)?;
        if stored.version != STORE_VERSION || !valid_session(&stored.session, now) {
            // An expired proof cannot be used again. Invalid content is retained
            // for diagnosis and reported as InvalidData instead of silently used.
            if stored.version == STORE_VERSION && stored.session.expires_at_ms <= now {
                remove_if_present(path)?;
                return Ok(None);
            }
            return Err(SessionStoreError::InvalidData);
        }
        Ok(Some(stored.session))
    }

    fn save_at(path: &Path, session: &UserSession, now: u64) -> Result<(), SessionStoreError> {
        if !valid_session(session, now) {
            return Err(SessionStoreError::InvalidData);
        }
        let parent = path.parent().ok_or(SessionStoreError::Io)?;
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(parent).map_err(|_| SessionStoreError::Io)?;
        // FAT does not enforce Unix mode bits, but this restricts access on
        // filesystems that support them without making FAT-based StockOS fail.
        let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));

        let encoded = serde_json::to_vec(&StoredSession {
            version: STORE_VERSION,
            session: session.clone(),
        })
        .map_err(|_| SessionStoreError::InvalidData)?;
        if encoded.len() as u64 > MAX_STORE_BYTES {
            return Err(SessionStoreError::InvalidData);
        }

        let temporary = parent.join(TEMP_FILE);
        remove_if_present(&temporary)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|_| SessionStoreError::Io)?;
        let write_result = (|| {
            file.write_all(&encoded)
                .map_err(|_| SessionStoreError::Io)?;
            file.sync_all().map_err(|_| SessionStoreError::Io)?;
            Ok(())
        })();
        drop(file);
        if let Err(error) = write_result {
            let _ = remove_if_present(&temporary);
            return Err(error);
        }
        fs::rename(&temporary, path).map_err(|_| {
            let _ = remove_if_present(&temporary);
            SessionStoreError::Io
        })?;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
        // Syncing the directory makes the rename durable where supported.
        // Some FAT drivers reject directory fsync, so it is best-effort.
        let _ = File::open(parent).and_then(|directory| directory.sync_all());
        Ok(())
    }

    pub fn load() -> Result<Option<UserSession>, SessionStoreError> {
        load_at(&data_dir()?.join(STORE_FILE), now_ms())
    }

    pub fn save(session: &UserSession) -> Result<(), SessionStoreError> {
        save_at(&data_dir()?.join(STORE_FILE), session, now_ms())
    }

    pub fn clear() -> Result<(), SessionStoreError> {
        let directory = data_dir()?;
        remove_if_present(&directory.join(STORE_FILE))?;
        remove_if_present(&directory.join(TEMP_FILE))
    }

    #[cfg(test)]
    mod tests {
        use super::{TEMP_FILE, load_at, save_at};
        use crate::backend::UserSession;
        use std::fs;
        use std::path::PathBuf;
        use std::time::{SystemTime, UNIX_EPOCH};

        fn test_dir(name: &str) -> PathBuf {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "brickwave-session-{name}-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            path
        }

        fn session(expires_at_ms: u64) -> UserSession {
            UserSession {
                session_id: "abcdefghijklmnopqrstuvwx01234567".to_owned(),
                session_secret: "0123456789abcdefghijklmnopqrstuvABCDEFGHIJK".to_owned(),
                expires_at_ms,
            }
        }

        #[test]
        fn unix_store_round_trips_and_replaces_atomically() {
            let directory = test_dir("round-trip");
            let path = directory.join("session.json");
            let first = session(20_000);
            save_at(&path, &first, 1_000).unwrap();
            assert_eq!(load_at(&path, 1_001).unwrap(), Some(first));

            let mut second = session(30_000);
            second.session_id = "ZYXWVUTSRQPONMLKJIHGFEDCBA987654".to_owned();
            save_at(&path, &second, 1_002).unwrap();
            assert_eq!(load_at(&path, 1_003).unwrap(), Some(second));
            assert!(!directory.join(TEMP_FILE).exists());
            fs::remove_dir_all(directory).unwrap();
        }

        #[test]
        fn unix_store_removes_expired_session() {
            let directory = test_dir("expired");
            let path = directory.join("session.json");
            save_at(&path, &session(2_000), 1_000).unwrap();
            assert_eq!(load_at(&path, 2_000).unwrap(), None);
            assert!(!path.exists());
            fs::remove_dir_all(directory).unwrap();
        }

        #[test]
        fn unix_store_rejects_malformed_or_oversized_content() {
            let directory = test_dir("invalid");
            let path = directory.join("session.json");
            fs::write(&path, br#"{"version":1,"session":{"session_id":"bad","session_secret":"bad","expires_at_ms":999999}}"#).unwrap();
            assert!(load_at(&path, 1_000).is_err());
            fs::write(&path, vec![b'x'; 16 * 1024 + 1]).unwrap();
            assert!(load_at(&path, 1_000).is_err());
            fs::remove_dir_all(directory).unwrap();
        }
    }
}

#[cfg(not(any(windows, unix)))]
mod platform {
    use super::{SessionStoreError, UserSession};

    pub fn load() -> Result<Option<UserSession>, SessionStoreError> {
        Err(SessionStoreError::PlatformUnavailable)
    }

    pub fn save(_session: &UserSession) -> Result<(), SessionStoreError> {
        Err(SessionStoreError::PlatformUnavailable)
    }

    pub fn clear() -> Result<(), SessionStoreError> {
        Err(SessionStoreError::PlatformUnavailable)
    }
}

pub use platform::{clear, load, save};
