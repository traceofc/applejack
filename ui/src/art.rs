// Artwork loader: memory cache (bounded by decoded size) -> disk cache -> HTTP.
// Fetching and decoding run on worker threads, so scrolling never waits on a JPEG;
// one shared HTTP agent keeps connections to the artwork CDN alive between images.
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
    rc::Rc,
    sync::LazyLock,
    time::Duration,
};

use gtk::{gdk, gio, glib, prelude::*};

/// Decoded textures kept in memory (RGBA bytes, roughly).
const MEM_BUDGET: usize = 96 * 1024 * 1024;

static AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .max_idle_connections_per_host(8)
        .build()
        .into()
});

type Waiter = Box<dyn FnOnce(&gdk::Texture)>;

pub struct ArtLoader {
    dir: PathBuf,
    mem: RefCell<HashMap<String, gdk::Texture>>,
    order: RefCell<VecDeque<String>>,
    mem_bytes: Cell<usize>,
    inflight: RefCell<HashMap<String, Vec<Waiter>>>,
}

impl ArtLoader {
    pub fn new(dir: PathBuf) -> Rc<Self> {
        let _ = std::fs::create_dir_all(&dir);
        Rc::new(Self {
            dir,
            mem: Default::default(),
            order: Default::default(),
            mem_bytes: Cell::new(0),
            inflight: Default::default(),
        })
    }

    /// Calls `done` with the texture once it is available (maybe immediately).
    pub fn load(self: &Rc<Self>, url: &str, done: impl FnOnce(&gdk::Texture) + 'static) {
        let hit = self.mem.borrow().get(url).cloned();
        if let Some(tex) = hit {
            done(&tex);
            return;
        }
        {
            let mut inflight = self.inflight.borrow_mut();
            if let Some(waiters) = inflight.get_mut(url) {
                waiters.push(Box::new(done));
                return;
            }
            inflight.insert(url.to_owned(), vec![Box::new(done)]);
        }
        let this = self.clone();
        let url = url.to_owned();
        let path = self.dir.join(file_name(&url));
        glib::spawn_future_local(async move {
            let u = url.clone();
            let tex = gio::spawn_blocking(move || {
                let bytes = fetch(&u, &path)?;
                gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes)).ok()
            })
            .await
            .ok()
            .flatten();
            let waiters = this.inflight.borrow_mut().remove(&url).unwrap_or_default();
            let Some(tex) = tex else { return };
            this.remember(url, tex.clone());
            for w in waiters {
                w(&tex);
            }
        });
    }

    /// Downloads artwork into the disk cache without decoding it, so a page that is
    /// likely to be opened next shows its images straight from disk.
    pub fn prefetch(self: &Rc<Self>, url: &str) {
        if self.mem.borrow().contains_key(url) || self.inflight.borrow().contains_key(url) {
            return;
        }
        let path = self.dir.join(file_name(url));
        if path.exists() {
            return;
        }
        let url = url.to_owned();
        gio::spawn_blocking(move || {
            fetch(&url, &path);
        });
    }

    fn remember(&self, url: String, tex: gdk::Texture) {
        let mut mem = self.mem.borrow_mut();
        let mut order = self.order.borrow_mut();
        let mut bytes = self.mem_bytes.get();
        if let Some(old) = mem.insert(url.clone(), tex.clone()) {
            bytes -= size_of_tex(&old);
        } else {
            order.push_back(url);
        }
        bytes += size_of_tex(&tex);
        while bytes > MEM_BUDGET && order.len() > 1 {
            let Some(old) = order.pop_front() else { break };
            if let Some(t) = mem.remove(&old) {
                bytes -= size_of_tex(&t);
            }
        }
        self.mem_bytes.set(bytes);
    }
}

fn size_of_tex(t: &gdk::Texture) -> usize {
    t.width().max(0) as usize * t.height().max(0) as usize * 4
}

fn file_name(url: &str) -> String {
    let mut h = DefaultHasher::new();
    url.hash(&mut h);
    format!("{:016x}.img", h.finish())
}

fn fetch(url: &str, path: &Path) -> Option<Vec<u8>> {
    if let Ok(b) = std::fs::read(path) {
        return Some(b);
    }
    let mut resp = AGENT.get(url).call().ok()?;
    let bytes = resp.body_mut().with_config().limit(16 * 1024 * 1024).read_to_vec().ok()?;
    // Write-then-rename: a concurrent reader never sees half a file.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp{}-{n}", std::process::id()));
    if std::fs::write(&tmp, &bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
    Some(bytes)
}
