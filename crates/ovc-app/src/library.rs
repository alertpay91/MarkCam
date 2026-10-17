//! The effect library: every ManyCam effect on disk (built-in `.mcep` packs + downloaded
//! `.mcv` category folders + imports), indexed on a background thread so the whole
//! collection can be browsed by category or searched by name, with lazily loaded
//! thumbnails.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use eframe::egui;
use ovc_mcam::{EffectRef, Kind};

/// Category name of the user's own images / GIFs.
pub const MY_EFFECTS: &str = "My effects";

/// The user's own organization of the library, kept apart from the effect files
/// (`%APPDATA%\MarkCam\library.json`): categories they created, and effects they moved
/// into another category.
#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
#[serde(default)]
pub struct Organization {
    /// User-created categories, in the order they were made.
    pub categories: Vec<String>,
    /// Effect key → category it was moved to.
    pub moves: BTreeMap<String, String>,
    /// Effects the user deleted from the browser (ManyCam's files are left alone; "Restore
    /// deleted effects" brings them back).
    pub hidden: BTreeSet<String>,
    /// ManyCam category → the name the user gave it.
    pub renamed: BTreeMap<String, String>,
}

fn org_path() -> PathBuf {
    crate::settings::app_dir().join("library.json")
}

impl Organization {
    fn load() -> Self {
        std::fs::read(org_path())
            .ok()
            .and_then(|b| serde_json::from_slice(b.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(&b)).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        let p = org_path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, serde_json::to_vec_pretty(self).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(tmp, p);
        }
    }
}

/// `%APPDATA%\MarkCam\My Effects`: everything added with "+" or drag & drop is copied here,
/// so it stays available (and shows up in the browser) across restarts.
pub fn my_effects_dir() -> PathBuf {
    crate::settings::app_dir().join("My Effects")
}

/// Whether to also list the effects of an installed ManyCam (Setup option, off by default:
/// MarkCam ships its own effects).
static USE_MANYCAM: AtomicBool = AtomicBool::new(false);

pub fn set_use_manycam(on: bool) {
    USE_MANYCAM.store(on, Ordering::SeqCst);
}

pub fn use_manycam() -> bool {
    USE_MANYCAM.load(Ordering::SeqCst)
}

/// MarkCam's own effect packs: one folder per category with PNG / GIF / APNG files (and
/// `*.face.json` placements for face stickers) — `<install dir>\effects\MarkCam`, or
/// `%APPDATA%\MarkCam\Packs` (development builds).
pub fn markcam_pack_dirs() -> Vec<PathBuf> {
    // The installed set; the development copy only when there is none.
    let installed = bundled_dir().map(|b| b.join("MarkCam")).filter(|d| d.is_dir());
    installed.or_else(|| Some(crate::settings::app_dir().join("Packs")).filter(|d| d.is_dir())).into_iter().collect()
}

/// Effects shipped with the installer: `<install dir>\effects` (`Effect Packs\*.mcep`,
/// `Video\<category>\*.mcv`, `My Effects\…`).
pub fn bundled_dir() -> Option<PathBuf> {
    let d = std::env::current_exe().ok()?.parent()?.join("effects");
    d.is_dir().then_some(d)
}

/// First start after installing: copy the bundled "My Effects" and category layout into
/// the user's data folder (never over what is already there).
pub fn seed_from_bundle() {
    let Some(b) = bundled_dir() else { return };
    let src = b.join("My Effects");
    let dst = my_effects_dir();
    if src.is_dir() && !dst.exists() && std::fs::create_dir_all(&dst).is_ok() {
        for e in std::fs::read_dir(&src).into_iter().flatten().flatten() {
            if e.path().is_file() {
                let _ = std::fs::copy(e.path(), dst.join(e.file_name()));
            }
        }
    }
    let org = b.join("library.json");
    if org.is_file() && !org_path().exists() {
        let _ = std::fs::create_dir_all(crate::settings::app_dir());
        let _ = std::fs::copy(org, org_path());
    }
}

/// Send a file to the Recycle Bin (so a deleted effect can still be got back).
#[cfg(windows)]
pub fn to_recycle_bin(p: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::UI::Shell::{SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE, SHFILEOPSTRUCTW};
    // Double-NUL-terminated list of paths.
    let mut from: Vec<u16> = p.as_os_str().encode_wide().collect();
    from.extend([0, 0]);
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: windows::core::PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO.0 | FOF_NOCONFIRMATION.0 | FOF_NOERRORUI.0 | FOF_SILENT.0) as u16,
        ..Default::default()
    };
    let r = unsafe { SHFileOperationW(&mut op) };
    if r == 0 && !p.exists() {
        Ok(())
    } else {
        std::fs::remove_file(p)
    }
}

/// Linux: the desktop's Trash (`gio trash`, GLib's tool, on every GTK desktop).
#[cfg(not(windows))]
pub fn to_recycle_bin(p: &Path) -> std::io::Result<()> {
    let trashed = std::process::Command::new("gio").arg("trash").arg(p).status().is_ok_and(|s| s.success());
    if trashed && !p.exists() {
        Ok(())
    } else {
        std::fs::remove_file(p)
    }
}

/// Video files in a folder, as "My effects" entries.
fn list_videos(dir: &std::path::Path) -> Vec<EffectRef> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    rd.filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && crate::video::is_video_file(p))
        .map(|p| EffectRef {
            pack_path: PathBuf::new(),
            pack_name: MY_EFFECTS.to_string(),
            mcv_name: String::new(),
            display_name: p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
            kind: Kind::Animation,
            file: Some(p),
        })
        .collect()
}

#[derive(Clone)]
pub struct Item {
    pub r: EffectRef,
    pub category: String,
    /// Stable identity (also the thumbnail cache key).
    pub key: String,
    /// Identity in the user's organization that doesn't depend on where the files are
    /// (`category/file`), so moves / deletions carry over to another computer.
    pub org_key: String,
    /// Lower-cased "name category" for search.
    haystack: String,
}

impl Item {
    fn new(r: EffectRef) -> Self {
        // Pack files are CamelCase ("FaceMasks.mcep") while the download folders are
        // spaced ("Face Masks"): split CamelCase so both land in one category.
        let category = if r.pack_name.contains(' ') {
            r.pack_name.clone()
        } else {
            let mut s = String::new();
            let mut prev_lower = false;
            for c in r.pack_name.chars() {
                if prev_lower && c.is_uppercase() {
                    s.push(' ');
                }
                prev_lower = c.is_lowercase();
                s.push(c);
            }
            s.replace(" And ", " & ")
        };
        let key = match &r.file {
            Some(p) => p.to_string_lossy().into_owned(),
            None => format!("{}|{}", r.pack_path.display(), r.mcv_name),
        };
        let entry = match &r.file {
            Some(p) => p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
            None => r.mcv_name.clone(),
        };
        let org_key = format!("{category}/{entry}");
        let haystack = format!("{} {}", r.display_name, category).to_lowercase();
        Item { r, category, key, org_key, haystack }
    }

    pub fn name(&self) -> &str {
        &self.r.display_name
    }

    pub fn kind(&self) -> Kind {
        self.r.kind
    }
}

pub struct Library {
    items: Arc<Mutex<Vec<Item>>>,
    /// Categories that come from MarkCam's own packs, in display order.
    own_categories: Arc<Mutex<Vec<String>>>,
    /// Effects left out of the browser: (encrypted by ManyCam, unsupported effect type).
    hidden: Arc<Mutex<(usize, usize)>>,
    scanning: Arc<AtomicBool>,
    /// Category → item count, rebuilt when a scan finishes.
    categories: Vec<(String, usize)>,
    seen_len: usize,
    /// Extra standalone `.mcv` files the user imported this session.
    imported: Vec<Item>,
    /// The user's categories and moved effects.
    org: Organization,
}

impl Library {
    /// Start indexing the default ManyCam folders (plus `extra_dir`, if any) in the background.
    pub fn start(extra_dir: Option<PathBuf>) -> Self {
        let items = Arc::new(Mutex::new(Vec::new()));
        let hidden = Arc::new(Mutex::new((0, 0)));
        let scanning = Arc::new(AtomicBool::new(true));
        let own_categories = Arc::new(Mutex::new(Vec::new()));
        std::thread::spawn({
            let (items, hidden, scanning, own) = (items.clone(), hidden.clone(), scanning.clone(), own_categories.clone());
            move || {
                let mut found = Vec::new();
                // ManyCam's folders only when asked for (Setup), plus a folder the user
                // pointed at.
                let manycam = use_manycam();
                let bundled = bundled_dir();
                let mut pack_dirs: Vec<PathBuf> = Vec::new();
                if manycam {
                    pack_dirs.extend(ovc_mcam::default_pack_dir());
                    pack_dirs.extend(bundled.as_ref().map(|b| b.join("Effect Packs")).filter(|d| d.is_dir()));
                }
                pack_dirs.extend(extra_dir);
                for d in pack_dirs {
                    for pack in ovc_mcam::list_packs(&d).unwrap_or_default() {
                        found.extend(ovc_mcam::list_effects_in_pack(&pack).unwrap_or_default().into_iter().map(Item::new));
                    }
                }
                let mut video_dirs: Vec<PathBuf> = Vec::new();
                if manycam {
                    video_dirs.extend(ovc_mcam::default_video_dir());
                    video_dirs.extend(bundled.as_ref().map(|b| b.join("Video")).filter(|d| d.is_dir()));
                }
                for vd in video_dirs {
                    for cat in ovc_mcam::list_category_dirs(&vd).unwrap_or_default() {
                        found.extend(ovc_mcam::list_effects_in_dir(&cat).unwrap_or_default().into_iter().map(Item::new));
                    }
                }
                found.sort_by(|a, b| a.name().to_lowercase().cmp(&b.name().to_lowercase()));
                // The same effect often ships in a built-in pack *and* sits in the downloaded
                // folders; show it once.
                found.dedup_by(|a, b| a.name().eq_ignore_ascii_case(b.name()) && a.kind() == b.kind());
                // Only show what can actually be used: encrypted (ManyCam-only) effects and
                // effect types we can't render yet would just be dead tiles.
                let locked = found.iter().filter(|i| i.kind() == Kind::Locked).count();
                let other = found.iter().filter(|i| i.kind() == Kind::Other).count();
                found.retain(|i| !matches!(i.kind(), Kind::Locked | Kind::Other));
                *hidden.lock().unwrap() = (locked, other);
                // MarkCam's own packs: one folder per category.
                for root in markcam_pack_dirs() {
                    let mut cats: Vec<PathBuf> = std::fs::read_dir(&root).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
                    cats.sort();
                    for cat in cats {
                        let name = cat.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                        // "01 Hats" → "Hats": a number prefix only sets the order.
                        let shown = name.trim_start_matches(|c: char| c.is_ascii_digit()).trim().to_string();
                        found.extend(ovc_mcam::list_images_in_dir(&cat, &shown).into_iter().map(Item::new));
                        let mut o = own.lock().unwrap();
                        if !o.contains(&shown) {
                            o.push(shown);
                        }
                    }
                }
                // The user's own images / GIFs / videos (never de-duplicated against ManyCam's).
                found.extend(ovc_mcam::list_images_in_dir(&my_effects_dir(), MY_EFFECTS).into_iter().map(Item::new));
                found.extend(list_videos(&my_effects_dir()).into_iter().map(Item::new));
                *items.lock().unwrap() = found;
                scanning.store(false, Ordering::SeqCst);
            }
        });
        Library { items, own_categories, hidden, scanning, categories: Vec::new(), seen_len: usize::MAX, imported: Vec::new(), org: Organization::load() }
    }

    /// How many effects were left out: (encrypted by ManyCam, unsupported type).
    pub fn hidden(&self) -> (usize, usize) {
        *self.hidden.lock().unwrap()
    }

    /// Categories of MarkCam's own effects.
    pub fn own_categories(&self) -> Vec<String> {
        self.own_categories.lock().unwrap().clone()
    }

    pub fn scanning(&self) -> bool {
        self.scanning.load(Ordering::SeqCst)
    }

    /// Copy an image / GIF into "My effects" (keeping a unique name) and list it there.
    pub fn add_my_effect(&mut self, path: &std::path::Path) -> std::io::Result<Item> {
        let dir = my_effects_dir();
        std::fs::create_dir_all(&dir)?;
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "effect".into());
        let ext = path.extension().map(|s| s.to_string_lossy().to_ascii_lowercase()).unwrap_or_else(|| "png".into());
        // Already inside My Effects: use it as is.
        let dst = if path.parent() == Some(dir.as_path()) {
            path.to_path_buf()
        } else {
            let mut dst = dir.join(format!("{stem}.{ext}"));
            let mut n = 2;
            while dst.exists() {
                dst = dir.join(format!("{stem} ({n}).{ext}"));
                n += 1;
            }
            std::fs::copy(path, &dst)?;
            dst
        };
        let r = ovc_mcam::list_images_in_dir(&dir, MY_EFFECTS)
            .into_iter()
            .chain(list_videos(&dir))
            .find(|r| r.file.as_deref() == Some(dst.as_path()))
            .ok_or_else(|| std::io::Error::other("not an image or video"))?;
        let item = Item::new(r);
        if !self.items.lock().unwrap().iter().any(|i| i.key == item.key) && !self.imported.iter().any(|i| i.key == item.key) {
            self.imported.push(item.clone());
            self.seen_len = usize::MAX;
        }
        Ok(item)
    }

    /// Add a `.mcv` the user imported by hand (shown under "Imported").
    pub fn add_imported(&mut self, path: PathBuf) -> Option<Item> {
        let stem = path.file_stem()?.to_string_lossy().into_owned();
        let mut r = ovc_mcam::list_effects_in_dir(path.parent()?)
            .ok()?
            .into_iter()
            .find(|r| r.file.as_deref() == Some(path.as_path()))
            .unwrap_or(EffectRef {
                pack_path: PathBuf::new(),
                pack_name: String::new(),
                mcv_name: String::new(),
                display_name: stem,
                kind: Kind::Other,
                file: Some(path.clone()),
            });
        r.pack_name = "Imported".into();
        let item = Item::new(r);
        self.imported.push(item.clone());
        self.seen_len = usize::MAX;
        Some(item)
    }

    /// Categories with their item counts (alphabetical); recomputed only when the library
    /// changes.
    pub fn categories(&mut self) -> &[(String, usize)] {
        let items = self.items.lock().unwrap();
        let len = items.len() + self.imported.len();
        if len != self.seen_len {
            let mut m: BTreeMap<String, usize> = BTreeMap::new();
            for c in &self.org.categories {
                m.entry(c.clone()).or_default(); // user categories show even when empty
            }
            for it in items.iter().chain(self.imported.iter()).filter(|i| !self.org.hidden.contains(&i.org_key)) {
                *m.entry(self.category_of(it).to_string()).or_default() += 1;
            }
            self.categories = m.into_iter().collect();
            self.seen_len = len;
        }
        drop(items);
        &self.categories
    }

    /// Effects shown (deleted ones not counted).
    pub fn total(&self) -> usize {
        let items = self.items.lock().unwrap();
        items.iter().chain(self.imported.iter()).filter(|i| !self.org.hidden.contains(&i.org_key)).count()
    }

    /// The category an effect is shown in: the user's move, else its own (under the name
    /// the user gave it, if renamed).
    pub fn category_of<'a>(&'a self, item: &'a Item) -> &'a str {
        self.org
            .moves
            .get(&item.org_key)
            .or_else(|| self.org.renamed.get(&item.category))
            .map(String::as_str)
            .unwrap_or(&item.category)
    }

    /// Items in one category.
    pub fn in_category(&self, cat: &str) -> Vec<Item> {
        let items = self.items.lock().unwrap();
        items
            .iter()
            .chain(self.imported.iter())
            .filter(|i| !self.org.hidden.contains(&i.org_key) && self.category_of(i) == cat)
            .cloned()
            .collect()
    }

    // ── The user's organization ─────────────────────────────────────────────────────

    /// Categories the user created.
    pub fn user_categories(&self) -> &[String] {
        &self.org.categories
    }

    pub fn is_user_category(&self, name: &str) -> bool {
        self.org.categories.iter().any(|c| c == name)
    }

    /// Whether the effect was moved out of its original category.
    pub fn is_moved(&self, item: &Item) -> bool {
        self.org.moves.contains_key(&item.org_key)
    }

    /// How many effects the user deleted from the browser (and can restore).
    pub fn deleted_count(&self) -> usize {
        self.org.hidden.len()
    }

    /// Bring back every deleted ManyCam effect and the original category names.
    pub fn restore_deleted(&mut self) {
        self.org.hidden.clear();
        self.org.renamed.clear();
        self.changed();
    }

    /// Delete an effect: the user's own file goes to the Recycle Bin; a ManyCam effect is
    /// only removed from the browser (its files belong to ManyCam).
    pub fn delete_item(&mut self, item: &Item) -> std::io::Result<()> {
        let mine = item.r.file.as_deref().is_some_and(|p| p.parent() == Some(my_effects_dir().as_path()));
        if mine {
            to_recycle_bin(item.r.file.as_deref().unwrap_or(Path::new("")))?;
            self.items.lock().unwrap().retain(|i| i.key != item.key);
            self.imported.retain(|i| i.key != item.key);
        } else {
            self.org.hidden.insert(item.org_key.clone());
        }
        self.org.moves.remove(&item.org_key);
        self.changed();
        Ok(())
    }

    fn changed(&mut self) {
        self.org.save();
        self.seen_len = usize::MAX;
    }

    /// Create a category; `false` if the name is empty or taken.
    pub fn create_category(&mut self, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() || self.categories().iter().any(|(c, _)| c.eq_ignore_ascii_case(name)) {
            return false;
        }
        self.org.categories.push(name.to_string());
        self.changed();
        true
    }

    /// Rename any category (yours or ManyCam's); effects moved into it follow.
    pub fn rename_category(&mut self, old: &str, new: &str) -> bool {
        let new = new.trim();
        if new.is_empty() || new == MY_EFFECTS || old == MY_EFFECTS {
            return false;
        }
        if new == old {
            return true;
        }
        if self.categories().iter().any(|(c, _)| c.eq_ignore_ascii_case(new) && c != old) {
            return false;
        }
        if self.is_user_category(old) {
            for c in self.org.categories.iter_mut().filter(|c| *c == old) {
                *c = new.to_string();
            }
        } else {
            // A ManyCam category: remember the new name for its original one.
            let orig = self.org.renamed.iter().find(|(_, v)| *v == old).map(|(k, _)| k.clone()).unwrap_or_else(|| old.to_string());
            if orig == new {
                self.org.renamed.remove(&orig);
            } else {
                self.org.renamed.insert(orig, new.to_string());
            }
        }
        for v in self.org.moves.values_mut().filter(|v| *v == old) {
            *v = new.to_string();
        }
        self.changed();
        true
    }

    /// Delete a category. Yours: its effects go back to where they came from. ManyCam's:
    /// its effects are removed from the browser (restorable).
    pub fn delete_category(&mut self, name: &str) {
        if self.is_user_category(name) {
            self.org.categories.retain(|c| c != name);
            self.org.moves.retain(|_, v| v != name);
        } else {
            for it in self.in_category(name) {
                self.org.hidden.insert(it.org_key.clone());
                self.org.moves.remove(&it.org_key);
            }
        }
        self.changed();
    }

    /// Move an effect to `cat`, or back to its own category with `None`.
    pub fn move_item(&mut self, item: &Item, cat: Option<&str>) {
        let own = self.org.renamed.get(&item.category).unwrap_or(&item.category).clone();
        match cat {
            Some(c) if c != own => {
                self.org.moves.insert(item.org_key.clone(), c.to_string());
            }
            _ => {
                self.org.moves.remove(&item.org_key);
            }
        }
        self.changed();
    }

    /// Search by name/category: every whitespace-separated word must match.
    pub fn search(&self, query: &str) -> Vec<Item> {
        let words: Vec<String> = query.to_lowercase().split_whitespace().map(str::to_owned).collect();
        if words.is_empty() {
            return Vec::new();
        }
        let items = self.items.lock().unwrap();
        items
            .iter()
            .chain(self.imported.iter())
            .filter(|i| !self.org.hidden.contains(&i.org_key))
            .filter(|i| words.iter().all(|w| i.haystack.contains(w.as_str())))
            .cloned()
            .collect()
    }
}

/// Lazily decoded effect thumbnails as GPU textures.
pub struct Thumbs {
    tx: Sender<(String, EffectRef)>,
    rx: Receiver<(String, Option<(Vec<u8>, u32, u32)>)>,
    /// `None` = requested / no thumbnail.
    tex: HashMap<String, Option<egui::TextureHandle>>,
}

impl Thumbs {
    pub fn start() -> Self {
        let (req_tx, req_rx) = channel::<(String, EffectRef)>();
        let (res_tx, res_rx) = channel();
        std::thread::spawn(move || {
            while let Ok((key, r)) = req_rx.recv() {
                let thumb = match r.file.as_deref().filter(|p| crate::video::is_video_file(p)) {
                    Some(p) => crate::video::first_frame(p),
                    None => ovc_mcam::load_thumbnail(&r),
                };
                if res_tx.send((key, thumb)).is_err() {
                    break;
                }
            }
        });
        Thumbs { tx: req_tx, rx: res_rx, tex: HashMap::new() }
    }

    /// Upload thumbnails decoded since the last frame.
    pub fn pump(&mut self, ctx: &egui::Context) {
        for _ in 0..24 {
            let Ok((key, img)) = self.rx.try_recv() else { break };
            let tex = img.map(|(rgba, w, h)| {
                let ci = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                ctx.load_texture(format!("thumb:{key}"), ci, egui::TextureOptions::LINEAR)
            });
            self.tex.insert(key, tex);
        }
    }

    /// The thumbnail for `item` if loaded; otherwise queue it (call only for visible tiles).
    pub fn get(&mut self, item: &Item) -> Option<&egui::TextureHandle> {
        if !self.tex.contains_key(&item.key) {
            self.tex.insert(item.key.clone(), None);
            let _ = self.tx.send((item.key.clone(), item.r.clone()));
        }
        self.tex.get(&item.key).and_then(|t| t.as_ref())
    }

    /// Drop a cached thumbnail (the file was rewritten).
    pub fn forget(&mut self, key: &str) {
        self.tex.remove(key);
    }

    /// Thumbnail by key (for the layers list), queuing it via `make_ref` if not yet loaded.
    pub fn get_for(&mut self, key: &str, make_ref: impl FnOnce() -> Option<EffectRef>) -> Option<&egui::TextureHandle> {
        if !self.tex.contains_key(key) {
            self.tex.insert(key.to_owned(), None);
            if let Some(r) = make_ref() {
                let _ = self.tx.send((key.to_owned(), r));
            }
        }
        self.tex.get(key).and_then(|t| t.as_ref())
    }
}

/// An [`EffectRef`] for a saved layer source (to fetch its thumbnail).
pub fn effect_ref_of(src: &crate::settings::LayerSource) -> Option<EffectRef> {
    use crate::settings::LayerSource as L;
    let blank = |pack_path: PathBuf, mcv_name: String, file: Option<PathBuf>| EffectRef {
        pack_path,
        pack_name: String::new(),
        mcv_name,
        display_name: String::new(),
        kind: Kind::Other,
        file,
    };
    match src {
        L::Image(p) => Some(blank(PathBuf::new(), String::new(), Some(p.clone()))),
        L::PackEntry { pack, mcv } => Some(blank(pack.clone(), mcv.clone(), None)),
        L::McvFile(p) | L::Video(p) => Some(blank(PathBuf::new(), String::new(), Some(p.clone()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Delete / rename / restore against the real ManyCam library, with the user's data
    /// redirected to a temp folder (OVC_DATA_DIR). Run alone: it sets an env var.
    #[test]
    #[ignore]
    fn organize() {
        let tmp = std::env::temp_dir().join("markcam-lib-test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("OVC_DATA_DIR", &tmp);
        let mine = my_effects_dir();
        std::fs::create_dir_all(&mine).unwrap();
        let png = image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 0, 0, 255]));
        png.save(mine.join("red.png")).unwrap();
        png.save(mine.join("red2.png")).unwrap();

        let mut lib = Library::start(None);
        while lib.scanning() {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let total = lib.total();
        let cats = lib.categories().to_vec();
        println!("{total} effects in {} categories", cats.len());
        assert_eq!(lib.in_category(MY_EFFECTS).len(), 2);

        // A ManyCam category: rename, then delete, then restore.
        let (cat, n) = cats.iter().find(|(c, n)| c != MY_EFFECTS && *n > 0).cloned().expect("ManyCam effects");
        assert!(lib.rename_category(&cat, "Renamed by test"));
        assert_eq!(lib.in_category("Renamed by test").len(), n);
        assert!(lib.in_category(&cat).is_empty());
        assert!(lib.rename_category("Renamed by test", "Renamed again"));
        assert_eq!(lib.in_category("Renamed again").len(), n);
        lib.delete_category("Renamed again");
        assert_eq!(lib.total(), total - n);
        assert_eq!(lib.deleted_count(), n);
        lib.restore_deleted();
        assert_eq!(lib.total(), total);
        assert_eq!(lib.in_category(&cat).len(), n);

        // One ManyCam effect: hidden, not deleted from disk.
        let it = lib.in_category(&cat)[0].clone();
        lib.delete_item(&it).unwrap();
        assert_eq!(lib.in_category(&cat).len(), n - 1);
        assert!(lib.search(it.name()).iter().all(|i| i.key != it.key));

        // A user category with a moved effect; rename keeps it, delete sends it back.
        assert!(lib.create_category("Lesson"));
        let red = lib.in_category(MY_EFFECTS).into_iter().find(|i| i.name() == "red").unwrap();
        lib.move_item(&red, Some("Lesson"));
        assert!(lib.rename_category("Lesson", "Lesson 2"));
        assert_eq!(lib.in_category("Lesson 2").len(), 1);
        lib.delete_category("Lesson 2");
        assert_eq!(lib.in_category(MY_EFFECTS).len(), 2);

        // Own file: to the Recycle Bin.
        lib.delete_item(&red).unwrap();
        assert!(!mine.join("red.png").exists());
        assert_eq!(lib.in_category(MY_EFFECTS).len(), 1);

        // Survives a restart (library.json).
        let lib2 = Library::start(None);
        while lib2.scanning() {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(lib2.in_category(&cat).len(), n - 1);
        println!("ok: renamed/deleted/restored “{cat}” ({n}), hid “{}”, recycled red.png", it.name());
    }
}
