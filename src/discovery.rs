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
    query_for(
        settings,
        &format!("STR(?url) > {}", sparql_string(after)),
        true,
    )
}
fn query_for(settings: &Settings, filter: &str, paged: bool) -> String {
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
    let limit = if paged { "LIMIT 256" } else { "" };
    format!(
        "PREFIX tracker: <http://tracker.api.gnome.org/ontology/v3/tracker#>
      PREFIX nfo: <http://tracker.api.gnome.org/ontology/v3/nfo#>
      PREFIX nie: <http://tracker.api.gnome.org/ontology/v3/nie#>
      SELECT DISTINCT ?url (tracker:id(?f) AS ?source) WHERE {{
      GRAPH tracker:FileSystem {{ ?f a nfo:FileDataObject ; nie:url ?url }}
      FILTER ({filter}) FILTER ({}) }} ORDER BY ?url {limit}",
        if scope.is_empty() { "false" } else { &scope }
    )
}

fn check_roots(settings: &Settings) -> Result<()> {
    for root in &settings.folders {
        fs::read_dir(root)
            .with_context(|| format!("Search location is unavailable: {}", root.display()))?;
    }
    Ok(())
}
fn connect(api: &Api) -> Result<glib::Object> {
    for service in [
        "org.freedesktop.LocalSearch3",
        "org.freedesktop.Tracker3.Miner.Files",
    ] {
        let name = CString::new(service)?;
        let mut error = ptr::null_mut();
        // SAFETY: valid C string; null optional arguments select the session bus/default endpoint.
        let object =
            unsafe { (api.connect)(name.as_ptr(), ptr::null(), ptr::null_mut(), &mut error) };
        let result = checked_error(error);
        if !object.is_null() {
            return Ok(unsafe { glib::Object::from_glib_full(object.cast()) });
        }
        if service.ends_with("Miner.Files") {
            result.context(
                "Cannot connect to GNOME LocalSearch. Enable file search in GNOME Settings.",
            )?;
        }
    }
    bail!("GNOME LocalSearch did not provide a connection")
}
fn read_query(
    api: &Api,
    connection: &glib::Object,
    query: &str,
    settings: &Settings,
    content_scope: bool,
) -> Result<(Vec<FileRecord>, String, usize)> {
    let query = CString::new(query)?;
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
    // SAFETY: the connection and cancellable remain alive for the query and cursor iteration.
    let cursor = unsafe {
        (api.query)(
            connection.as_ptr().cast(),
            query.as_ptr(),
            cancel.to_glib_none().0,
            &mut error,
        )
    };
    checked_error(error)?;
    if cursor.is_null() {
        bail!("LocalSearch returned no cursor");
    }
    let cursor_object = unsafe { glib::Object::from_glib_full(cursor.cast()) };
    let mut records = vec![];
    let mut after = String::new();
    let mut read = 0;
    loop {
        error = ptr::null_mut();
        let more = unsafe { (api.next)(cursor, cancel.to_glib_none().0, &mut error) };
        checked_error(error)?;
        if more == 0 {
            break;
        }
        // SAFETY: the cursor is on a valid row. Copy borrowed values before advancing it.
        let raw = unsafe { (api.string_at)(cursor, 0, ptr::null_mut()) };
        if raw.is_null() {
            bail!("LocalSearch returned a row without a file URI");
        }
        let uri = unsafe { CStr::from_ptr(raw) }
            .to_string_lossy()
            .into_owned();
        after = uri.clone();
        read += 1;
        let record = if content_scope {
            record_from_uri(&uri, settings)?
        } else {
            record_for_search(&uri, settings)?
        };
        if let Some(mut record) = record {
            let id = unsafe { (api.string_at)(cursor, 1, ptr::null_mut()) };
            if id.is_null() {
                bail!("LocalSearch returned no resource ID");
            }
            record.source_id = unsafe { CStr::from_ptr(id) }.to_str()?.parse()?;
            records.push(record);
        }
    }
    unsafe { (api.close)(cursor) };
    drop(cursor_object);
    let _ = done_tx.send(());
    Ok((records, after, read))
}

/// Initial/periodic recovery snapshots are paged. Normal updates query only the changed
/// resource IDs supplied by LocalSearch's GraphUpdated signal, including renames/deletions.
pub fn reconcile(
    settings: &Settings,
    mut page: impl FnMut(Vec<FileRecord>) -> Result<()>,
) -> Result<usize> {
    if settings.folders.is_empty() {
        return Ok(0);
    }
    check_roots(settings)?;
    let api = Api::load()?;
    let connection = connect(api)?;
    let mut after = String::new();
    let mut count = 0;
    loop {
        let (records, last, read) = read_query(
            api,
            &connection,
            &discovery_query(settings, &after),
            settings,
            true,
        )?;
        after = last;
        count += records.len();
        page(records)?;
        if read < 256 {
            break;
        }
    }
    Ok(count)
}
pub fn changed(settings: &Settings, ids: &[i64]) -> Result<Vec<FileRecord>> {
    check_roots(settings)?;
    let api = Api::load()?;
    let connection = connect(api)?;
    let mut records = vec![];
    for chunk in ids.chunks(256) {
        let ids = chunk
            .iter()
            .filter(|id| **id > 0)
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        if ids.is_empty() {
            continue;
        }
        let query = query_for(settings, &format!("tracker:id(?f) IN ({ids})"), false);
        records.extend(read_query(api, &connection, &query, settings, true)?.0);
    }
    Ok(records)
}

pub fn record_from_uri(uri: &str, settings: &Settings) -> Result<Option<FileRecord>> {
    record_checked(uri, |path| settings.allows(path))
}

pub fn record_for_search(uri: &str, settings: &Settings) -> Result<Option<FileRecord>> {
    record_checked(uri, |path| settings.allows_filename(path))
}

/// Read GNOME's filename index directly. These records never enter GSeek's
/// embedding queue, vector database, or content readers.
pub fn filename_matches(query: &str, settings: &Settings, limit: usize) -> Result<Vec<FileRecord>> {
    let terms: Vec<_> = query
        .split_whitespace()
        .filter(|t| t.chars().any(char::is_alphanumeric))
        .take(32)
        .collect();
    if terms.is_empty() {
        return Ok(vec![]);
    }
    let filter = terms
        .iter()
        .map(|t| {
            format!(
                "CONTAINS(LCASE(nfo:fileName(?f)), LCASE({}))",
                sparql_string(t)
            )
        })
        .collect::<Vec<_>>()
        .join(" && ");
    // TinySPARQL uses whitespace to AND prefix terms. Restrict each term to
    // alphanumeric text so FTS operators and punctuation stay out of its grammar.
    let pattern = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .take(32)
        .map(|s| format!("{s}*"))
        .collect::<Vec<_>>()
        .join(" ");
    if pattern.is_empty() {
        return Ok(vec![]);
    }

    // Retrieve exact names/titles independently of the broad URI-paged pool.
    // An exact file in Downloads must not disappear behind thousands of weak
    // prefix matches just because its URI sorts later.
    let exact = format!(
        "LCASE(nfo:fileName(?f)) = LCASE({0}) || LCASE(REPLACE(nfo:fileName(?f), {1}, \"\")) = LCASE({0})",
        sparql_string(query.trim()),
        sparql_string(r"\.[^.]+$")
    );
    let mut records = filename_records(None, &exact, settings, limit)?;
    records.extend(filename_records(Some(&pattern), &filter, settings, limit)?);
    let mut seen = std::collections::HashSet::new();
    let mut ranked: Vec<_> = records
        .into_iter()
        .filter(|file| seen.insert(file.uri.clone()))
        .map(|file| {
            (
                crate::ranking::filename(query, &file.name, &file.path),
                file,
            )
        })
        .collect();
    ranked.sort_by(|(a, left), (b, right)| b.total_cmp(a).then(left.uri.cmp(&right.uri)));
    Ok(ranked
        .into_iter()
        .take(limit)
        .map(|(_, file)| file)
        .collect())
}

pub fn indexed_search_record(uri: &str, settings: &Settings) -> Result<Option<FileRecord>> {
    if record_for_search(uri, settings)?.is_none() {
        return Ok(None);
    }
    Ok(
        filename_records(None, &format!("?url = {}", sparql_string(uri)), settings, 1)?
            .into_iter()
            .next(),
    )
}

fn filename_records(
    pattern: Option<&str>,
    filter: &str,
    settings: &Settings,
    limit: usize,
) -> Result<Vec<FileRecord>> {
    let api = Api::load()?;
    let connection = connect(api)?;
    let candidate = pattern
        .map(|p| format!("?f fts:match {} .", sparql_string(p)))
        .unwrap_or_default();
    let mut after = String::new();
    let mut eligible = vec![];
    loop {
        let query = format!(
            "PREFIX tracker: <http://tracker.api.gnome.org/ontology/v3/tracker#>
        PREFIX fts: <http://tracker.api.gnome.org/ontology/v3/fts#>
        PREFIX nfo: <http://tracker.api.gnome.org/ontology/v3/nfo#>
        PREFIX nie: <http://tracker.api.gnome.org/ontology/v3/nie#>
        SELECT DISTINCT ?url (tracker:id(?f) AS ?source) WHERE {{
        GRAPH tracker:FileSystem {{ ?f a nfo:FileDataObject ; nie:url ?url . {candidate} }}
        FILTER ({filter}) FILTER (STR(?url) > {}) }} ORDER BY ?url LIMIT 256",
            sparql_string(&after)
        );
        let (records, last, read) = read_query(api, &connection, &query, settings, false)?;
        eligible.extend(records);
        if read < 256 || eligible.len() >= limit {
            break;
        }
        after = last;
    }
    eligible.truncate(limit);
    Ok(eligible)
}

fn record_checked(uri: &str, allowed: impl Fn(&Path) -> bool) -> Result<Option<FileRecord>> {
    let Ok(url) = url::Url::parse(uri) else {
        return Ok(None);
    };
    let Ok(path) = url.to_file_path() else {
        return Ok(None);
    };
    if !allowed(&path) {
        return Ok(None);
    }
    let meta = match fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            return Ok(None);
        }
        Err(e) => return Err(e.into()),
    };
    if !(meta.is_file() || meta.is_dir()) || meta.file_type().is_symlink() {
        return Ok(None);
    }
    let canonical = match fs::canonicalize(&path) {
        Ok(path) => path,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            return Ok(None);
        }
        Err(e) => return Err(e.into()),
    };
    if !allowed(&canonical) {
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
        source_id: 0,
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

pub fn notification_ids(parameters: &glib::Variant) -> Option<Vec<i64>> {
    if parameters.type_().as_str() != "(sa{ii})" {
        return None;
    }
    // TinySPARQL sends (event type, resource ID), with repeated event types. A
    // HashMap would silently drop all but one creation/update in each batch.
    Some(
        parameters
            .child_value(1)
            .iter()
            .filter_map(|entry| entry.child_value(1).get::<i32>().map(i64::from))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn notifications_preserve_every_resource_in_a_batch() -> Result<()> {
        let message =
            glib::Variant::parse(None, "('tracker:FileSystem', {0: 12, 0: 34, 1: 56, 2: 78})")?;
        assert_eq!(notification_ids(&message), Some(vec![12, 34, 56, 78]));
        assert_eq!(notification_ids(&glib::Variant::from("unexpected")), None);
        Ok(())
    }
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
