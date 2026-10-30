//! Chrome extensions the user added. Packages (.crx from the Web Store or anywhere else, or a
//! .zip of an extension folder) are unpacked by our own CRX/ZIP/DEFLATE code into the vault's
//! settings directory, one folder per extension, and Chromium loads the enabled ones at startup
//! (`--load-extension`). Changes apply on the next start.

mod crx;
mod inflate;
mod zip;

pub use crx::id_from;

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

/// Packages bigger than this are refused outright.
const MAX_PACKAGE: usize = 128 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Extension {
    /// Chrome's extension ID (the Web Store's, for .crx packages).
    pub id: String,
    /// The folder under the manager's directory.
    pub dir: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub enabled: bool,
    /// Manifest-relative path of the icon to show.
    pub icon: Option<String>,
    /// Manifest-relative path of the toolbar popup, if it has one.
    pub popup: Option<String>,
    pub installed: u64,
}

pub struct Manager {
    dir: PathBuf,
    list: Vec<Extension>,
}

impl Manager {
    /// Opens (creating if needed) the extensions directory.
    pub fn open(dir: PathBuf) -> Self {
        let list = fs::read(dir.join("extensions.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Self { dir, list }
    }

    pub fn list(&self) -> &[Extension] {
        &self.list
    }

    /// Folders Chromium should load: the enabled extensions that are on disk.
    pub fn load_paths(&self) -> Vec<PathBuf> {
        self.list.iter().filter(|e| e.enabled).map(|e| self.dir.join(&e.dir)).filter(|p| p.join("manifest.json").is_file()).collect()
    }

    fn save(&self) {
        let _ = fs::create_dir_all(&self.dir);
        let json = serde_json::to_vec_pretty(&self.list).expect("extension list serializes");
        let tmp = self.dir.join("extensions.json.tmp");
        if let Err(e) = fs::write(&tmp, json).and_then(|_| fs::rename(&tmp, self.dir.join("extensions.json"))) {
            log::warn!("extensions: saving the list failed: {e}");
        }
    }

    /// Installs (or updates) the extension in a .crx or .zip package.
    pub fn install(&mut self, data: &[u8]) -> Result<Extension, String> {
        if data.len() > MAX_PACKAGE {
            return Err("that package is too big".into());
        }
        let (zip, key) = if data.starts_with(b"Cr24") {
            let crx = crx::parse(data)?;
            (crx.zip, Some(crx.public_key))
        } else if data.starts_with(b"PK") {
            (data, None)
        } else {
            return Err("not an extension package (expected .crx or .zip)".into());
        };
        let mut entries = zip::read(zip).map_err(|e| e.to_string())?;

        // A zipped folder: everything sits under one top-level directory.
        if !entries.iter().any(|e| e.path == "manifest.json") {
            let prefix = entries
                .iter()
                .find_map(|e| e.path.strip_suffix("manifest.json").filter(|p| p.matches('/').count() == 1).map(str::to_owned))
                .ok_or("no manifest.json in the package")?;
            if !entries.iter().all(|e| e.path.starts_with(&prefix)) {
                return Err("no manifest.json at the top of the package".into());
            }
            for entry in &mut entries {
                entry.path = entry.path[prefix.len()..].to_owned();
            }
        }

        let manifest_bytes = &entries.iter().find(|e| e.path == "manifest.json").expect("checked above").data;
        let mut manifest: Value = serde_json::from_slice(strip_bom(manifest_bytes)).map_err(|e| format!("manifest.json: {e}"))?;
        match manifest["manifest_version"].as_u64() {
            Some(3) => {}
            Some(2) => return Err("Manifest V2 extensions no longer run in Chromium 156; look for a newer (V3) version".into()),
            _ => return Err("manifest.json has no valid manifest_version".into()),
        }
        if manifest["name"].as_str().is_none() || manifest["version"].as_str().is_none() {
            return Err("manifest.json needs a name and a version".into());
        }

        // The ID: from the publisher key (Web Store packages, or a `key` already in the manifest),
        // else from the folder path, as Chrome does for unpacked extensions.
        let key = key.or_else(|| manifest["key"].as_str().and_then(|k| B64.decode(k.trim()).ok()));
        let (id, folder) = match &key {
            Some(key) => {
                let id = crx::id_from(key);
                (id.clone(), id)
            }
            None => {
                let folder = format!("local-{}", &crx::id_from(format!("{}{}", manifest["name"], now_nanos()).as_bytes())[..16]);
                (path_id(&self.dir.join(&folder)), folder)
            }
        };
        if let Some(key) = &key {
            // Keeps the ID when Chromium loads the folder unpacked.
            manifest["key"] = Value::String(B64.encode(key));
        }

        let messages = locale_messages(&manifest, &entries);
        let text = |field: &str| localize(manifest[field].as_str().unwrap_or_default(), &messages);
        let existing = self.list.iter().position(|e| e.id == id);
        let extension = Extension {
            id: id.clone(),
            dir: existing.map_or(folder, |i| self.list[i].dir.clone()),
            name: text("name"),
            version: manifest["version"].as_str().unwrap_or_default().to_owned(),
            description: text("description"),
            enabled: existing.is_none_or(|i| self.list[i].enabled),
            icon: best_icon(&manifest),
            popup: manifest["action"]["default_popup"].as_str().map(str::to_owned),
            installed: now_nanos() as u64 / 1_000_000_000,
        };

        // Unpack next to the old copy, then swap.
        let target = self.dir.join(&extension.dir);
        let staging = self.dir.join(format!("{}.new", extension.dir));
        let _ = fs::remove_dir_all(&staging);
        for entry in &entries {
            let path = staging.join(&entry.path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let data = if entry.path == "manifest.json" {
                serde_json::to_vec_pretty(&manifest).expect("manifest serializes")
            } else {
                entry.data.clone()
            };
            fs::write(&path, data).map_err(|e| e.to_string())?;
        }
        let _ = fs::remove_dir_all(&target);
        fs::rename(&staging, &target).map_err(|e| e.to_string())?;

        match existing {
            Some(i) => self.list[i] = extension.clone(),
            None => self.list.push(extension.clone()),
        }
        self.save();
        log::info!("extensions: installed {} {} ({})", extension.name, extension.version, extension.id);
        Ok(extension)
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) {
        if let Some(e) = self.list.iter_mut().find(|e| e.id == id) {
            e.enabled = enabled;
            self.save();
        }
    }

    pub fn remove(&mut self, id: &str) {
        if let Some(i) = self.list.iter().position(|e| e.id == id) {
            let e = self.list.remove(i);
            let _ = fs::remove_dir_all(self.dir.join(&e.dir));
            self.save();
        }
    }

    /// The extension's icon as a `data:` URL, for the extensions page.
    pub fn icon_data_url(&self, id: &str) -> Option<String> {
        let e = self.list.iter().find(|e| e.id == id)?;
        let icon = e.icon.as_ref()?;
        let path = self.dir.join(&e.dir).join(icon.trim_start_matches('/'));
        let mime = if icon.ends_with(".svg") { "image/svg+xml" } else { "image/png" };
        Some(format!("data:{mime};base64,{}", B64.encode(fs::read(path).ok()?)))
    }
}

fn strip_bom(b: &[u8]) -> &[u8] {
    b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(b)
}

fn now_nanos() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos())
}

/// Chrome's ID for an unpacked extension at `path` (hash of the path's bytes as stored).
fn path_id(path: &Path) -> String {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let bytes: Vec<u8> = path.as_os_str().encode_wide().flat_map(u16::to_le_bytes).collect();
        crx::id_from(&bytes)
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::ffi::OsStrExt;
        crx::id_from(path.as_os_str().as_bytes())
    }
}

/// `messages.json` of the default locale, keys lowercased.
fn locale_messages(manifest: &Value, entries: &[zip::Entry]) -> serde_json::Map<String, Value> {
    let Some(locale) = manifest["default_locale"].as_str() else { return Default::default() };
    let path = format!("_locales/{locale}/messages.json");
    entries
        .iter()
        .find(|e| e.path == path)
        .and_then(|e| serde_json::from_slice::<serde_json::Map<String, Value>>(strip_bom(&e.data)).ok())
        .map(|m| m.into_iter().map(|(k, v)| (k.to_lowercase(), v)).collect())
        .unwrap_or_default()
}

/// `__MSG_appName__` → the default locale's text.
fn localize(text: &str, messages: &serde_json::Map<String, Value>) -> String {
    match text.strip_prefix("__MSG_").and_then(|t| t.strip_suffix("__")) {
        Some(key) => messages.get(&key.to_lowercase()).and_then(|m| m["message"].as_str()).unwrap_or(text).to_owned(),
        None => text.to_owned(),
    }
}

/// The icon closest to 128 px (largest below, else smallest above).
fn best_icon(manifest: &Value) -> Option<String> {
    let icons = manifest["icons"].as_object()?;
    let mut sizes: Vec<(u32, &str)> =
        icons.iter().filter_map(|(size, path)| Some((size.parse().ok()?, path.as_str()?))).collect();
    sizes.sort();
    sizes.iter().rev().find(|(s, _)| *s <= 128).or(sizes.first()).map(|(_, p)| p.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("unskilled-ext-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn installs_crx_with_web_store_id() {
        let dir = temp("crx");
        let mut m = Manager::open(dir.clone());
        let e = m.install(include_bytes!("../tests/fixtures/ext.crx")).unwrap();
        assert_eq!(e.id, "bjabnkbmjpgjjleipglcgdgogfmlphdk");
        assert_eq!(e.name, "Test Ext");
        assert_eq!(e.description, "A test");
        assert_eq!(e.icon.as_deref(), Some("icons/48.png"));
        let manifest: Value = serde_json::from_slice(&fs::read(dir.join(&e.dir).join("manifest.json")).unwrap()).unwrap();
        assert!(manifest["key"].as_str().is_some(), "the key keeps the ID when loaded unpacked");
        assert!(m.icon_data_url(&e.id).unwrap().starts_with("data:image/png;base64,"));

        // Reopening reads the list back; disable and remove work.
        let mut m = Manager::open(dir.clone());
        assert_eq!(m.load_paths(), vec![dir.join(&e.dir)]);
        m.set_enabled(&e.id, false);
        assert!(m.load_paths().is_empty());
        m.remove(&e.id);
        assert!(m.list().is_empty() && !dir.join(&e.dir).exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn installs_zip_unpacked() {
        let dir = temp("zip");
        let mut m = Manager::open(dir.clone());
        let e = m.install(include_bytes!("../tests/fixtures/ext.zip")).unwrap();
        assert!(e.dir.starts_with("local-"));
        assert_eq!(e.id, path_id(&dir.join(&e.dir)));
        assert!(dir.join(&e.dir).join("content/cs.js").is_file());
        let _ = fs::remove_dir_all(dir);
    }

    /// A real package: `UNSKILLED_TEST_CRX=/path/to.crx cargo test -p unskilled-extensions -- --ignored`.
    #[test]
    #[ignore]
    fn real_crx() {
        let path = std::env::var("UNSKILLED_TEST_CRX").expect("UNSKILLED_TEST_CRX");
        let dir = temp("real");
        let mut m = Manager::open(dir.clone());
        let e = m.install(&fs::read(path).unwrap()).unwrap();
        println!("{e:?}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn refuses_junk() {
        let mut m = Manager::open(temp("junk"));
        assert!(m.install(b"hello").is_err());
    }
}
