// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
pub(crate) fn extract_site(root: &str) -> Result<std::path::PathBuf, String> {
    use std::cell::RefCell;
    day_core::tls_group! {
    static DONE: RefCell<std::collections::HashMap<String, std::path::PathBuf>> =
        RefCell::new(std::collections::HashMap::new());
    }
    if let Some(dir) = DONE.with(|m| m.borrow().get(root).cloned()) {
        return Ok(dir);
    }
    let app = gtk4::glib::prgname().unwrap_or_else(|| "day-app".into());
    let dest = gtk4::glib::user_cache_dir()
        .join("day-web")
        .join(app.as_str())
        .join(root);
    // Overwrite-extract once per process: stale caches from an older app build must not linger,
    // and the cost is one pass over a bundled site's files. An empty root is the whole asset
    // tree, which is what a site reading the app's own files (`app_assets`) browses.
    let _ = std::fs::remove_dir_all(&dest);
    let res_dir = match root {
        "" => "/day/assets".to_string(),
        root => format!("/day/assets/{root}"),
    };
    extract_tree(&res_dir, &dest)?;
    DONE.with(|m| m.borrow_mut().insert(root.to_string(), dest.clone()));
    Ok(dest)
}

/// Recursively copy one GResource directory (`res_dir`, absolute resource path) into `dest`.
fn extract_tree(res_dir: &str, dest: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("mkdir {}: {e}", dest.display()))?;
    let children =
        gtk4::gio::resources_enumerate_children(res_dir, gtk4::gio::ResourceLookupFlags::NONE)
            .map_err(|e| format!("enumerate {res_dir}: {e}"))?;
    for child in children {
        let name = child.as_str();
        if let Some(dir_name) = name.strip_suffix('/') {
            extract_tree(&format!("{res_dir}/{dir_name}"), &dest.join(dir_name))?;
        } else {
            let bytes = gtk4::gio::resources_lookup_data(
                &format!("{res_dir}/{name}"),
                gtk4::gio::ResourceLookupFlags::NONE,
            )
            .map_err(|e| format!("read {res_dir}/{name}: {e}"))?;
            std::fs::write(dest.join(name), bytes.as_ref())
                .map_err(|e| format!("write {}: {e}", dest.join(name).display()))?;
        }
    }
    Ok(())
}
