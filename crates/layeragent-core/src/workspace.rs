//! Incremental workspace change tracking.
//!
//! A [`Tracker`] keeps a baseline taken when a turn starts and a current view that is
//! refreshed after every tool call that can modify files. On Linux an inotify instance is
//! read synchronously at refresh time: the kernel queues an event during the very write
//! that the finished tool performed, so nothing is lost to thread scheduling, and only the
//! reported paths are re-hashed. Everywhere else, and whenever the queue overflows or a
//! watch cannot be established, refresh falls back to a stat walk that re-hashes only files
//! whose (mtime, size, inode) changed. The turn-final diff always uses the stat walk.
//!
//! Attribution is by observation window: a change seen between two refreshes may also come
//! from another person or program. Coverage limits are reported with every result.
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
use tokio_util::sync::CancellationToken;

const MAX_ENTRIES: usize = 100_000;
const MAX_HASHED: u64 = 1_073_741_824;
const MAX_FILE: u64 = 134_217_728;
const MAX_TEXT_FILE: u64 = 1_048_576;
const MAX_TEXT_TOTAL: usize = 33_554_432;
const DIFF_LIMIT: usize = 4_194_304;
/// Files modified this recently are re-hashed even if their stamp matches (coarse mtimes).
const RACY_WINDOW: Duration = Duration::from_secs(2);
#[cfg(target_os = "linux")]
const MAX_WATCHES: usize = 16_384;
const EXCLUDED: [&str; 7] = [
    ".git",
    ".agents",
    "target",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
];

#[derive(Clone, PartialEq, Debug)]
struct Stamp {
    mtime_ns: i128,
    size: u64,
    inode: u64,
}

#[derive(Clone)]
struct FileState {
    hash: String,
    bytes: u64,
    text: Option<String>,
    stamp: Stamp,
}

#[derive(Default)]
pub(super) struct Snapshot {
    files: BTreeMap<PathBuf, FileState>,
    skipped: usize,
    scan_limited: bool,
}

fn stamp_of(meta: &std::fs::Metadata) -> Stamp {
    #[cfg(unix)]
    let inode = std::os::unix::fs::MetadataExt::ino(meta);
    #[cfg(not(unix))]
    let inode = 0;
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as i128);
    Stamp {
        mtime_ns,
        size: meta.len(),
        inode,
    }
}

fn excluded_name(name: &str) -> bool {
    layeragent_config::is_secret_path_component(name) || EXCLUDED.contains(&name)
}

fn trackable(relative: &Path) -> bool {
    relative
        .components()
        .all(|c| !excluded_name(&c.as_os_str().to_string_lossy()))
}

/// What a refresh found: previous states of changed/removed paths, and the paths that now
/// exist with new content.
#[derive(Default)]
pub(super) struct Delta {
    old: BTreeMap<PathBuf, FileState>,
    new: BTreeSet<PathBuf>,
}

pub(super) struct Tracker {
    root: PathBuf,
    baseline: Snapshot,
    current: Snapshot,
    hashed: u64,
    retained: usize,
    #[cfg(target_os = "linux")]
    watcher: Option<watch::Watcher>,
}

fn read_state(
    path: &Path,
    meta: &std::fs::Metadata,
    budget: &mut Budget,
    cancel: &CancellationToken,
) -> Result<FileState> {
    let size = meta.len();
    ensure!(size <= MAX_FILE, "file exceeds capture limit");
    ensure!(budget.hashed + size <= MAX_HASHED, "hash budget exhausted");
    let file = std::fs::File::open(path)?;
    let mut reader = file.take(MAX_FILE.min(MAX_HASHED - budget.hashed) + 1);
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    let mut text = Vec::new();
    let keep_text = size <= MAX_TEXT_FILE && budget.retained + size as usize <= MAX_TEXT_TOTAL;
    let mut buffer = [0u8; 65_536];
    loop {
        ensure!(!cancel.is_cancelled(), "workspace capture cancelled");
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        ensure!(bytes <= MAX_FILE, "file grew beyond capture limit");
        hash.update(&buffer[..n]);
        if keep_text && text.len() + n <= MAX_TEXT_FILE as usize {
            text.extend_from_slice(&buffer[..n]);
        }
    }
    let text = if keep_text && bytes == text.len() as u64 && !text.contains(&0) {
        String::from_utf8(text).ok()
    } else {
        None
    };
    if let Some(t) = &text {
        budget.retained += t.len();
    }
    budget.hashed += bytes;
    Ok(FileState {
        hash: hex::encode(hash.finalize()),
        bytes,
        text,
        stamp: stamp_of(meta),
    })
}

struct Budget {
    hashed: u64,
    retained: usize,
}

impl Tracker {
    /// Takes the baseline with a full scan.
    pub(super) fn start(root: &Path, cancel: &CancellationToken) -> Result<Self> {
        let mut tracker = Self {
            root: root.to_path_buf(),
            baseline: Snapshot::default(),
            current: Snapshot::default(),
            hashed: 0,
            retained: 0,
            #[cfg(target_os = "linux")]
            watcher: None,
        };
        tracker.walk(cancel)?;
        #[cfg(target_os = "linux")]
        {
            // Watches are established after the scan; anything that changes in between is
            // caught by the stat walk the final diff always performs.
            tracker.watcher = watch::Watcher::new(&tracker.root);
        }
        tracker.baseline = Snapshot {
            files: tracker.current.files.clone(),
            skipped: tracker.current.skipped,
            scan_limited: tracker.current.scan_limited,
        };
        Ok(tracker)
    }

    /// Stat walk that re-hashes only files whose stamp changed.
    fn walk(&mut self, cancel: &CancellationToken) -> Result<Delta> {
        let started = SystemTime::now();
        let mut previous = std::mem::take(&mut self.current.files);
        let mut next = BTreeMap::new();
        let mut delta = Delta::default();
        let mut budget = Budget {
            hashed: 0,
            retained: 0,
        };
        let (mut skipped, mut limited, mut entries) = (0usize, false, 0usize);
        let root = self.root.clone();
        let walker = walkdir::WalkDir::new(&root)
            .follow_links(false)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || !excluded_name(&e.file_name().to_string_lossy()));
        for entry in walker {
            ensure!(!cancel.is_cancelled(), "workspace capture cancelled");
            entries += 1;
            if entries > MAX_ENTRIES || budget.hashed >= MAX_HASHED {
                limited = true;
                break;
            }
            let Ok(entry) = entry else {
                skipped += 1;
                continue;
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                skipped += 1;
                continue;
            };
            if meta.len() > MAX_FILE {
                skipped += 1;
                continue;
            }
            let relative = entry.path().strip_prefix(&root)?.to_path_buf();
            let stamp = stamp_of(&meta);
            let fresh_enough = started
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as i128)
                - stamp.mtime_ns
                > RACY_WINDOW.as_nanos() as i128;
            if fresh_enough && previous.get(&relative).is_some_and(|p| p.stamp == stamp) {
                let kept = previous.remove(&relative).expect("checked above");
                budget.hashed += kept.bytes;
                budget.retained += kept.text.as_ref().map_or(0, String::len);
                next.insert(relative, kept);
                continue;
            }
            match read_state(entry.path(), &meta, &mut budget, cancel) {
                Ok(state) => {
                    let old = previous.remove(&relative);
                    if old.as_ref().is_none_or(|o| o.hash != state.hash) {
                        if let Some(o) = old {
                            delta.old.insert(relative.clone(), o);
                        }
                        delta.new.insert(relative.clone());
                    }
                    next.insert(relative, state);
                }
                Err(_) => skipped += 1,
            }
        }
        for (path, state) in previous {
            // Absent from this walk: deleted, or hidden by a limit we already reported.
            if !limited {
                delta.old.insert(path, state);
            } else {
                next.insert(path, state);
            }
        }
        self.hashed = budget.hashed;
        self.retained = budget.retained;
        self.current = Snapshot {
            files: next,
            skipped,
            scan_limited: limited,
        };
        Ok(delta)
    }

    /// Applies one reported path: re-hash a file, drop a vanished path, or walk a new directory.
    fn apply_path(&mut self, relative: &Path, delta: &mut Delta, cancel: &CancellationToken) {
        if !trackable(relative) {
            return;
        }
        let full = self.root.join(relative);
        match std::fs::symlink_metadata(&full) {
            Err(_) => {
                let gone: Vec<PathBuf> = self
                    .current
                    .files
                    .keys()
                    .filter(|k| k.starts_with(relative))
                    .cloned()
                    .collect();
                for key in gone {
                    if let Some(old) = self.current.files.remove(&key) {
                        delta.new.remove(&key);
                        delta.old.entry(key).or_insert(old);
                    }
                }
            }
            Ok(meta) if meta.is_file() => {
                let mut budget = Budget {
                    hashed: self.hashed,
                    retained: self.retained,
                };
                match read_state(&full, &meta, &mut budget, cancel) {
                    Ok(state) => {
                        self.hashed = budget.hashed;
                        self.retained = budget.retained;
                        let old = self.current.files.insert(relative.to_path_buf(), state);
                        let changed = old
                            .as_ref()
                            .zip(self.current.files.get(relative))
                            .is_none_or(|(o, n)| o.hash != n.hash);
                        if changed {
                            if let Some(o) = old {
                                delta.old.entry(relative.to_path_buf()).or_insert(o);
                            }
                            delta.new.insert(relative.to_path_buf());
                        }
                    }
                    Err(_) => self.current.skipped += 1,
                }
            }
            Ok(meta) if meta.is_dir() => {
                for entry in walkdir::WalkDir::new(&full)
                    .follow_links(false)
                    .into_iter()
                    .filter_entry(|e| {
                        e.depth() == 0 || !excluded_name(&e.file_name().to_string_lossy())
                    })
                    .flatten()
                {
                    if entry.file_type().is_file()
                        && let Ok(rel) = entry.path().strip_prefix(&self.root)
                    {
                        let rel = rel.to_path_buf();
                        self.apply_path(&rel, delta, cancel);
                    }
                }
            }
            Ok(_) => {}
        }
    }

    /// Changes since the previous refresh. Uses inotify when it is healthy.
    pub(super) fn refresh(&mut self, cancel: &CancellationToken) -> Result<Value> {
        let delta = self.refresh_delta(cancel)?;
        Ok(self.render_delta(&delta))
    }

    fn refresh_delta(&mut self, cancel: &CancellationToken) -> Result<Delta> {
        #[cfg(target_os = "linux")]
        if let Some(watcher) = self.watcher.as_mut()
            && let Some(dirty) = watcher.drain(&self.root)
        {
            let mut delta = Delta::default();
            for path in dirty {
                self.apply_path(&path, &mut delta, cancel);
            }
            return Ok(delta);
        }
        #[cfg(target_os = "linux")]
        {
            // Overflow or no watcher: walk, then try to re-arm the watcher.
            let delta = self.walk(cancel)?;
            self.watcher = watch::Watcher::new(&self.root);
            Ok(delta)
        }
        #[cfg(not(target_os = "linux"))]
        self.walk(cancel)
    }

    fn render_delta(&self, delta: &Delta) -> Value {
        let keys: BTreeSet<&PathBuf> = delta.old.keys().chain(delta.new.iter()).collect();
        render(
            keys.into_iter().map(|k| {
                let old = delta.old.get(k);
                let new = delta
                    .new
                    .contains(k)
                    .then(|| self.current.files.get(k))
                    .flatten();
                (k.as_path(), old, new)
            }),
            &self.current,
            &self.current,
        )
    }

    /// Everything that differs from the turn's baseline, from a fresh authoritative walk.
    pub(super) fn finish(&mut self, cancel: &CancellationToken) -> Result<Value> {
        self.walk(cancel)?;
        Ok(changes(&self.baseline, &self.current))
    }

    #[cfg(test)]
    pub(super) fn watching(&self) -> bool {
        #[cfg(target_os = "linux")]
        return self.watcher.is_some();
        #[cfg(not(target_os = "linux"))]
        false
    }
}

#[cfg(target_os = "linux")]
mod watch {
    use super::*;
    use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask};
    use std::collections::HashMap;

    pub(super) struct Watcher {
        inotify: Inotify,
        dirs: HashMap<WatchDescriptor, PathBuf>,
    }

    const MASK: WatchMask = WatchMask::CREATE
        .union(WatchMask::DELETE)
        .union(WatchMask::MODIFY)
        .union(WatchMask::CLOSE_WRITE)
        .union(WatchMask::MOVED_FROM)
        .union(WatchMask::MOVED_TO)
        .union(WatchMask::ATTRIB)
        .union(WatchMask::DELETE_SELF)
        .union(WatchMask::MOVE_SELF);

    impl Watcher {
        /// Watches every trackable directory; `None` if that is not possible.
        pub(super) fn new(root: &Path) -> Option<Self> {
            let mut watcher = Self {
                inotify: Inotify::init().ok()?,
                dirs: HashMap::new(),
            };
            watcher.add_tree(root, root).then_some(watcher)
        }

        fn add_tree(&mut self, root: &Path, dir: &Path) -> bool {
            for entry in walkdir::WalkDir::new(dir)
                .follow_links(false)
                .into_iter()
                .filter_entry(|e| {
                    e.depth() == 0 || !excluded_name(&e.file_name().to_string_lossy())
                })
                .flatten()
            {
                if !entry.file_type().is_dir() {
                    continue;
                }
                if self.dirs.len() >= MAX_WATCHES {
                    return false;
                }
                let Ok(wd) = self.inotify.watches().add(entry.path(), MASK) else {
                    return false;
                };
                if let Ok(relative) = entry.path().strip_prefix(root) {
                    self.dirs.insert(wd, relative.to_path_buf());
                }
            }
            true
        }

        /// Reports paths (relative to `root`) touched since the last drain, or `None` when the
        /// event stream can no longer be trusted (overflow, unreadable queue, lost watch).
        pub(super) fn drain(&mut self, root: &Path) -> Option<BTreeSet<PathBuf>> {
            let mut dirty = BTreeSet::new();
            let mut new_dirs = Vec::new();
            let mut buffer = [0u8; 16_384];
            loop {
                let events = match self.inotify.read_events(&mut buffer) {
                    Ok(events) => events,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => return None,
                };
                let mut any = false;
                for event in events {
                    any = true;
                    if event.mask.contains(EventMask::Q_OVERFLOW) {
                        return None;
                    }
                    let Some(base) = self.dirs.get(&event.wd) else {
                        // IGNORED for a watch we already forgot.
                        continue;
                    };
                    let path = match event.name {
                        Some(name) => base.join(name),
                        None => base.clone(),
                    };
                    if event.mask.contains(EventMask::IGNORED) {
                        dirty.insert(base.clone());
                        continue;
                    }
                    if event.mask.contains(EventMask::ISDIR)
                        && event
                            .mask
                            .intersects(EventMask::CREATE | EventMask::MOVED_TO)
                    {
                        new_dirs.push(path.clone());
                    }
                    dirty.insert(path);
                }
                if !any {
                    break;
                }
            }
            for dir in new_dirs {
                // A directory that appeared may already contain files created before the watch.
                if trackable(&dir) && !self.add_tree(root, &root.join(&dir)) {
                    return None;
                }
            }
            Some(dirty)
        }
    }
}

/// Renders changed entries as a manifest plus a bounded unified diff.
fn render<'a>(
    entries: impl Iterator<Item = (&'a Path, Option<&'a FileState>, Option<&'a FileState>)>,
    a: &Snapshot,
    b: &Snapshot,
) -> Value {
    let (mut diff, mut files, mut truncated) = (String::new(), Vec::new(), false);
    for (path, old, new) in entries {
        if old.map(|s| &s.hash) == new.map(|s| &s.hash) {
            continue;
        }
        let kind = if old.is_none() {
            "added"
        } else if new.is_none() {
            "deleted"
        } else {
            "modified"
        };
        files.push(json!({"path":path.to_string_lossy(),"kind":kind,"before_sha256":old.map(|s| &s.hash),"after_sha256":new.map(|s| &s.hash),"before_bytes":old.map(|s| s.bytes),"after_bytes":new.map(|s| s.bytes)}));
        let old_text = old.map(|s| s.text.as_deref()).unwrap_or(Some(""));
        let new_text = new.map(|s| s.text.as_deref()).unwrap_or(Some(""));
        if diff.len() >= DIFF_LIMIT {
            truncated = true;
            continue;
        }
        match (old_text, new_text) {
            (Some(x), Some(y)) => {
                let rendered = similar::TextDiff::from_lines(x, y)
                    .unified_diff()
                    .header(
                        &format!("a/{}", path.display()),
                        &format!("b/{}", path.display()),
                    )
                    .to_string();
                if diff.len() + rendered.len() <= DIFF_LIMIT {
                    diff.push_str(&rendered);
                } else {
                    truncated = true;
                }
            }
            _ => diff.push_str(&format!(
                "{kind}: {} (binary or large file; see SHA-256 manifest)\n",
                path.display()
            )),
        }
    }
    json!({"diff":diff,"files":files,"coverage":{"before_skipped":a.skipped,"after_skipped":b.skipped,"scan_limited":a.scan_limited || b.scan_limited,"text_diff_truncated":truncated,"limits":{"files":MAX_ENTRIES,"file_bytes":MAX_FILE,"hashed_bytes":MAX_HASHED,"text_bytes":MAX_TEXT_TOTAL},"excludes":"secrets, protected metadata, dependency/build directories and symlinks; changes can include concurrent external edits"}})
}

pub(super) fn changes(before: &Snapshot, after: &Snapshot) -> Value {
    let keys: BTreeSet<&PathBuf> = before.files.keys().chain(after.files.keys()).collect();
    render(
        keys.into_iter()
            .map(|k| (k.as_path(), before.files.get(k), after.files.get(k))),
        before,
        after,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token() -> CancellationToken {
        CancellationToken::new()
    }

    fn paths(value: &Value) -> Vec<String> {
        value["files"]
            .as_array()
            .map(|files| {
                files
                    .iter()
                    .map(|f| {
                        format!(
                            "{}:{}",
                            f["kind"].as_str().unwrap_or(""),
                            f["path"].as_str().unwrap_or("")
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn binary_changes_are_hashed_and_existing_user_changes_are_not_attributed() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(
            root.path().join("user.txt"),
            "pre-existing uncommitted work",
        )?;
        std::fs::write(root.path().join("binary"), [0, 1, 2])?;
        std::fs::create_dir(root.path().join(".ssh"))?;
        std::fs::write(root.path().join(".ssh/id_rsa"), "secret")?;
        let mut tracker = Tracker::start(root.path(), &token())?;
        std::fs::write(root.path().join("binary"), [0, 1, 3])?;
        let result = tracker.finish(&token())?;
        assert_eq!(paths(&result), vec!["modified:binary"]);
        assert!(
            !tracker
                .baseline
                .files
                .contains_key(Path::new(".ssh/id_rsa"))
        );
        Ok(())
    }

    #[test]
    fn per_call_refresh_reports_only_what_changed_since_the_last_one() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("a.txt"), "one\n")?;
        std::fs::create_dir(root.path().join("src"))?;
        std::fs::write(root.path().join("src/lib.rs"), "fn a() {}\n")?;
        let mut tracker = Tracker::start(root.path(), &token())?;
        assert!(
            paths(&tracker.refresh(&token())?).is_empty(),
            "nothing happened yet"
        );

        std::fs::write(root.path().join("a.txt"), "two\n")?;
        let first = tracker.refresh(&token())?;
        assert_eq!(paths(&first), vec!["modified:a.txt"]);
        assert!(first["diff"].as_str().unwrap_or("").contains("+two"));

        std::fs::remove_file(root.path().join("src/lib.rs"))?;
        std::fs::create_dir_all(root.path().join("new/deep"))?;
        std::fs::write(root.path().join("new/deep/f.txt"), "x")?;
        let second = tracker.refresh(&token())?;
        let mut got = paths(&second);
        got.sort();
        assert_eq!(got, vec!["added:new/deep/f.txt", "deleted:src/lib.rs"]);

        // Cumulative view is still against the turn baseline.
        let total = tracker.finish(&token())?;
        let mut all = paths(&total);
        all.sort();
        assert_eq!(
            all,
            vec![
                "added:new/deep/f.txt",
                "deleted:src/lib.rs",
                "modified:a.txt"
            ]
        );
        Ok(())
    }

    #[test]
    fn file_created_inside_a_new_directory_is_not_missed() -> Result<()> {
        let root = tempfile::tempdir()?;
        let mut tracker = Tracker::start(root.path(), &token())?;
        #[cfg(target_os = "linux")]
        assert!(
            tracker.watching(),
            "inotify path must be the one under test"
        );
        // The directory and its content appear within one observation window.
        std::fs::create_dir(root.path().join("d"))?;
        std::fs::write(root.path().join("d/inside.txt"), "hello")?;
        assert_eq!(
            paths(&tracker.refresh(&token())?),
            vec!["added:d/inside.txt"]
        );
        // Later edits in that directory are seen through the newly added watch.
        std::fs::write(root.path().join("d/inside.txt"), "hello world")?;
        assert_eq!(
            paths(&tracker.refresh(&token())?),
            vec!["modified:d/inside.txt"]
        );
        Ok(())
    }

    #[test]
    fn same_size_rewrite_within_the_racy_window_is_still_detected() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("f"), "aaaa")?;
        let mut tracker = Tracker::start(root.path(), &token())?;
        std::fs::write(root.path().join("f"), "bbbb")?;
        // Force the stat-walk path regardless of inotify.
        #[cfg(target_os = "linux")]
        {
            tracker.watcher = None;
        }
        assert_eq!(paths(&tracker.refresh(&token())?), vec!["modified:f"]);
        Ok(())
    }

    #[test]
    fn excluded_and_secret_paths_are_never_reported() -> Result<()> {
        let root = tempfile::tempdir()?;
        let mut tracker = Tracker::start(root.path(), &token())?;
        std::fs::write(root.path().join(".env"), "SECRET=1")?;
        std::fs::create_dir(root.path().join("node_modules"))?;
        std::fs::write(root.path().join("node_modules/x.js"), "1")?;
        std::fs::write(root.path().join("ok.txt"), "1")?;
        assert_eq!(paths(&tracker.refresh(&token())?), vec!["added:ok.txt"]);
        Ok(())
    }
}
