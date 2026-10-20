use crate::config::Paths;
use anyhow::{Result, bail};
use gio::prelude::*;
use std::path::Path;

pub fn icon_for(path: &Path, mime: &str) -> gio::Icon {
    let file = gio::File::for_path(path);
    if let Ok(info) = file.query_info(
        "standard::icon,thumbnail::path,thumbnail::is-valid",
        gio::FileQueryInfoFlags::NONE,
        gio::Cancellable::NONE,
    ) {
        if info.boolean("thumbnail::is-valid")
            && let Some(thumb) = info.attribute_byte_string("thumbnail::path")
        {
            let thumb = Path::new(thumb.as_str());
            if thumb.is_file() {
                return gio::FileIcon::new(&gio::File::for_path(thumb)).upcast();
            }
        }
        if let Some(icon) = info.icon() {
            return icon;
        }
    }
    gio::content_type_get_icon(mime)
}

pub fn open_result(paths: &Paths, uri: &str) -> Result<()> {
    let settings = paths.load()?;
    let stored = crate::store::Store::open(&paths.database())?
        .file(uri)?
        .is_some()
        && crate::discovery::record_from_uri(uri, &settings)?.is_some();
    if !stored && crate::discovery::indexed_search_record(uri, &settings)?.is_none() {
        bail!("This result moved or is outside your search locations");
    }
    gio::AppInfo::launch_default_for_uri(uri, None::<&gio::AppLaunchContext>)?;
    Ok(())
}
