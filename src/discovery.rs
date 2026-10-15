//! A small, read-only adapter to GNOME's supported TinySPARQL C API.
//!
//! Dynamic linking allows the same application bundle to use the host's LocalSearch client.
//! The older Tracker 3 library has the same ABI. No filesystem crawler is used.
use crate::{config::Settings, store::FileRecord};
use anyhow::{Context, Result, bail};
use gio::prelude::*;
use glib::translate::{FromGlibPtrFull, ToGlibPtr};
use libloading::Library;
use std::{
    ffi::{CStr, CString, c_char, c_void},
    fs,
    os::unix::fs::MetadataExt,
    path::Path,
    ptr,
};

type Object = *mut c_void;
type Error = *mut *mut glib::ffi::GError;
type Connect = unsafe extern "C" fn(
    *const c_char,
    *const c_char,
    *mut gio::ffi::GDBusConnection,
    Error,
) -> Object;
type Query =
    unsafe extern "C" fn(Object, *const c_char, *mut gio::ffi::GCancellable, Error) -> Object;
type Next = unsafe extern "C" fn(Object, *mut gio::ffi::GCancellable, Error) -> i32;
type StringAt = unsafe extern "C" fn(Object, i32, *mut libc::c_long) -> *const c_char;
type Close = unsafe extern "C" fn(Object);

struct Api {
    _library: Library,
    connect: Connect,
    query: Query,
    next: Next,
    string_at: StringAt,
    close: Close,
}
impl Api {
    fn load() -> Result<&'static Self> {
        // GObject types remain registered for the life of the process. Unloading/reloading
        // their implementation library would try to register the same types a second time.
        static API: std::sync::OnceLock<Result<Api, String>> = std::sync::OnceLock::new();
        API.get_or_init(|| Self::load_once().map_err(|e| format!("{e:#}")))
            .as_ref()
            .map_err(|e| anyhow::anyhow!("{e}"))
    }
    fn load_once() -> Result<Self> {
        for name in ["libtinysparql-3.0.so.0", "libtracker-sparql-3.0.so.0"] {
            // SAFETY: only the documented system SONAMEs and stable C ABI are loaded. The Library
            // lives at least as long as every function pointer and GObject created by this adapter.
            unsafe {
                if let Ok(lib) = Library::new(name) {
                    return Ok(Self {
                        connect: *lib.get(b"tracker_sparql_connection_bus_new\0")?,
                        query: *lib.get(b"tracker_sparql_connection_query\0")?,
                        next: *lib.get(b"tracker_sparql_cursor_next\0")?,
                        string_at: *lib.get(b"tracker_sparql_cursor_get_string\0")?,
                        close: *lib.get(b"tracker_sparql_cursor_close\0")?,
                        _library: lib,
                    });
                }
            }
        }
        bail!(
            "GNOME LocalSearch is unavailable. Install your distribution's LocalSearch/TinySPARQL package, then open Settings → Search → Search Locations."
        );
    }
}

fn checked_error(error: *mut glib::ffi::GError) -> Result<()> {
    if !error.is_null() {
        // SAFETY: GError is returned with transfer-full ownership by GLib.
        let error = unsafe { glib::Error::from_glib_full(error) };
        return Err(error.into());
    }
    Ok(())
}

pub fn sparql_string(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t")
    )
}

pub fn discovery_query(settings: &Settings, after: &str) -> String {
    let scope = settings
        .folders
        .iter()
        .filter_map(|p| url::Url::from_file_path(p).ok())
        .map(|u| {
            let s = u.as_str().trim_end_matches('/');
            format!(
                "(?url = {} || STRSTARTS(STR(?url), {}))",
                sparql_string(s),
                sparql_string(&format!("{s}/"))
            )
        })
        .collect::<Vec<_>>()
        .join(" || ");
    format!("PREFIX tracker: <http://tracker.api.gnome.org/ontology/v3/tracker#>
        PREFIX nfo: <http://tracker.api.gnome.org/ontology/v3/nfo#>
        PREFIX nie: <http://tracker.api.gnome.org/ontology/v3/nie#>
        SELECT DISTINCT ?url WHERE {{ GRAPH tracker:FileSystem {{ ?f a nfo:FileDataObject ; nie:url ?url }}
        FILTER (STR(?url) > {}) FILTER ({}) }} ORDER BY ?url LIMIT 256",sparql_string(after),if scope.is_empty(){"false"}else{&scope})
}

pub fn reconcile(
    settings: &Settings,
    mut page: impl FnMut(Vec<FileRecord>) -> Result<()>,
) -> Result<usize> {
    if settings.folders.is_empty() {
        return Ok(0);
    }
    // An unavailable mount must never be mistaken for deletion of all the files in it.
    for root in &settings.folders {
        fs::read_dir(root)
            .with_context(|| format!("Search location is unavailable: {}", root.display()))?;
    }
    let api = Api::load()?;
    let mut connection = None;
    for service in [
        "org.freedesktop.LocalSearch3",
        "org.freedesktop.Tracker3.Miner.Files",
    ] {
        let name = CString::new(service)?;
        let mut error = ptr::null_mut();
        // SAFETY: inputs are valid C strings; null optional parameters select GLib's session bus.
        let object =
            unsafe { (api.connect)(name.as_ptr(), ptr::null(), ptr::null_mut(), &mut error) };
        let result = checked_error(error);
        if !object.is_null() {
            // SAFETY: bus_new returns a GObject with a full reference.
            connection = Some(unsafe { glib::Object::from_glib_full(object.cast()) });
            break;
        }
        if service.ends_with("Miner.Files") {
            result.context(
                "Cannot connect to GNOME LocalSearch. Enable file search in GNOME Settings.",
            )?;
        }
    }
    let connection = connection.context("GNOME LocalSearch did not provide a connection")?;
    let conn_ptr = connection.as_ptr().cast();
    let mut after = String::new();
    let mut count = 0;
    loop {
        let query = CString::new(discovery_query(settings, &after))?;
        let mut error = ptr::null_mut();
        let cancel = gio::Cancellable::new();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let timeout_cancel = cancel.clone();
        std::thread::spawn(move || {
            if done_rx
                .recv_timeout(std::time::Duration::from_secs(15))
                .is_err()
            {
                timeout_cancel.cancel();
            }
        });
        // SAFETY: the connection and cancellable remain alive during the synchronous query.
        let cursor = unsafe {
            (api.query)(
                conn_ptr,
                query.as_ptr(),
                cancel.to_glib_none().0,
                &mut error,
            )
        };
        checked_error(error)?;
        if cursor.is_null() {
            bail!("LocalSearch returned no cursor");
        }
        // SAFETY: the cursor has transfer-full GObject ownership and outlives all reads below.
        let cursor_object = unsafe { glib::Object::from_glib_full(cursor.cast()) };
        let mut records = vec![];
        let mut read = 0;
        loop {
            error = ptr::null_mut();
            // SAFETY: cursor is live; next moves to a valid row before string_at is used.
            let more = unsafe { (api.next)(cursor, cancel.to_glib_none().0, &mut error) };
            checked_error(error)?;
            if more == 0 {
                break;
            }
            let raw = unsafe { (api.string_at)(cursor, 0, ptr::null_mut()) };
            if raw.is_null() {
                bail!("LocalSearch returned a row without a file URI");
            }
            // SAFETY: the borrowed UTF-8 string is copied before the cursor advances.
            let uri = unsafe { CStr::from_ptr(raw) }
                .to_string_lossy()
                .into_owned();
            after = uri.clone();
            read += 1;
            if let Some(record) = record_from_uri(&uri, settings)? {
                records.push(record);
            }
        }
        unsafe { (api.close)(cursor) };
        drop(cursor_object);
        let _ = done_tx.send(());
        count += records.len();
        page(records)?;
        if read < 256 {
            break;
        }
    }
    Ok(count)
}

pub fn record_from_uri(uri: &str, settings: &Settings) -> Result<Option<FileRecord>> {
    let Ok(url) = url::Url::parse(uri) else {
        return Ok(None);
    };
    let Ok(path) = url.to_file_path() else {
        return Ok(None);
    };
    if !settings.allows(&path) {
        return Ok(None);
    }
    let meta = match fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if !(meta.is_file() || meta.is_dir()) || meta.file_type().is_symlink() {
        return Ok(None);
    }
    if !settings.allows(&fs::canonicalize(&path)?) {
        return Ok(None);
    }
    let (content_type, _) = gio::content_type_guess(Some(&path), None::<&[u8]>);
    let mime = if meta.is_dir() {
        "inode/directory".into()
    } else {
        gio::content_type_get_mime_type(&content_type)
            .map(|s| s.to_string())
            .unwrap_or_else(|| "application/octet-stream".into())
    };
    Ok(Some(FileRecord {
        name: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        uri: uri.into(),
        stamp: format!(
            "{}:{}:{}:{}:{}",
            meta.dev(),
            meta.ino(),
            meta.mtime(),
            meta.mtime_nsec(),
            meta.len()
        ),
        size: meta.len(),
        folder: meta.is_dir(),
        path,
        mime,
    }))
}

pub fn root_is_visible(root: &Path) -> bool {
    fs::read_dir(root).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sparql_never_interpolates_unescaped_user_paths() {
        assert_eq!(sparql_string("a\"\\\n"), "\"a\\\"\\\\\\n\"");
        let s = Settings {
            folders: vec!["/home/user/My Documents".into()],
            ..Default::default()
        };
        let q = discovery_query(&s, "file:///a\"} UNION {?a ?b ?c}");
        assert!(q.contains("My%20Documents/"));
        assert!(q.contains("a\\\"}"));
    }
    #[test]
    fn symlinks_and_non_files_are_not_read() -> Result<()> {
        let d = tempfile::Builder::new().prefix("gseek-test-").tempdir()?;
        let target = d.path().join("target.txt");
        fs::write(&target, "hi")?;
        let link = d.path().join("link.txt");
        std::os::unix::fs::symlink(&target, &link)?;
        let s = Settings {
            folders: vec![d.path().into()],
            ..Default::default()
        };
        assert!(record_from_uri(url::Url::from_file_path(target).unwrap().as_str(), &s)?.is_some());
        assert!(record_from_uri(url::Url::from_file_path(link).unwrap().as_str(), &s)?.is_none());
        assert!(record_from_uri("https://example.com/file.txt", &s)?.is_none());
        Ok(())
    }
}
