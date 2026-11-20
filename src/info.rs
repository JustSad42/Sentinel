//! Lazily loaded per-process metadata (description, publisher, user, command line) and icons,
//! fetched on a worker thread so the UI never blocks.

use crate::sampler::Key;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex};

pub type Rgba = Arc<(u32, u32, Vec<u8>)>;

#[derive(Default)]
pub struct Meta {
    pub description: Option<String>,
    pub company: Option<String>,
    pub user: Option<String>,
    pub elevated: Option<bool>,
    pub cmdline: Option<String>,
    pub icon_key: Option<String>,
}

#[derive(Clone, Default)]
struct PathMeta {
    description: Option<String>,
    company: Option<String>,
}

enum Job {
    Proc(Key, Option<Arc<str>>),
    Icon(String),
}

#[derive(Default)]
struct State {
    by_key: HashMap<Key, Arc<Meta>>,
    by_path: HashMap<String, PathMeta>,
    icons: HashMap<String, Option<Rgba>>,
    queue: VecDeque<Job>,
    pending: HashSet<Key>,
    pending_icons: HashSet<String>,
    ctx: Option<egui::Context>,
}

pub struct Info {
    st: Mutex<State>,
    cv: Condvar,
}

impl Info {
    pub fn new() -> Arc<Self> {
        let me = Arc::new(Self { st: Mutex::new(State::default()), cv: Condvar::new() });
        let m = me.clone();
        std::thread::Builder::new()
            .name("info".into())
            .spawn(move || {
                unsafe { windows_sys::Win32::System::Com::CoInitializeEx(std::ptr::null(), windows_sys::Win32::System::Com::COINIT_APARTMENTTHREADED as u32) };
                m.worker()
            })
            .ok();
        me
    }

    pub fn set_ctx(&self, ctx: egui::Context) {
        self.st.lock().unwrap().ctx = Some(ctx);
    }

    fn worker(&self) {
        let mut done = 0u32;
        loop {
            let job = {
                let mut st = self.st.lock().unwrap();
                while st.queue.is_empty() {
                    if done > 0 {
                        if let Some(c) = &st.ctx {
                            c.request_repaint();
                        }
                        done = 0;
                    }
                    st = self.cv.wait(st).unwrap();
                }
                st.queue.pop_front().unwrap()
            };
            match job {
                Job::Proc(key, path) => {
                    let pm = path.as_ref().map(|p| {
                        let cached = self.st.lock().unwrap().by_path.get(&**p).cloned();
                        cached.unwrap_or_else(|| {
                            let (d, c) = crate::win::version_strings(p);
                            let pm = PathMeta { description: d, company: c };
                            self.st.lock().unwrap().by_path.insert(p.to_string(), pm.clone());
                            pm
                        })
                    });
                    let meta = Meta {
                        description: pm.as_ref().and_then(|m| m.description.clone()),
                        company: pm.as_ref().and_then(|m| m.company.clone()),
                        user: crate::win::process_user(key.0),
                        elevated: crate::win::process_is_elevated(key.0),
                        cmdline: crate::win::command_line(key.0),
                        icon_key: path.as_ref().map(|p| p.to_string()),
                    };
                    if let Some(p) = &path {
                        self.load_icon(p);
                    }
                    let mut st = self.st.lock().unwrap();
                    st.by_key.insert(key, Arc::new(meta));
                    st.pending.remove(&key);
                }
                Job::Icon(spec) => self.load_icon(&spec),
            }
            done += 1;
            if done >= 24 {
                if let Some(c) = &self.st.lock().unwrap().ctx {
                    c.request_repaint();
                }
                done = 0;
            }
        }
    }

    fn load_icon(&self, spec: &str) {
        if self.st.lock().unwrap().icons.contains_key(spec) {
            return;
        }
        let img = if spec.contains(',') || spec.to_ascii_lowercase().ends_with(".ico") {
            crate::win::resource_icon_rgba(spec)
        } else {
            crate::win::file_icon_rgba(spec, 32)
        };
        let mut st = self.st.lock().unwrap();
        st.icons.insert(spec.to_string(), img.map(Arc::new));
        st.pending_icons.remove(spec);
    }

    pub fn meta(&self, key: Key, path: Option<&Arc<str>>) -> Option<Arc<Meta>> {
        let mut st = self.st.lock().unwrap();
        if let Some(m) = st.by_key.get(&key) {
            return Some(m.clone());
        }
        if st.pending.insert(key) {
            st.queue.push_back(Job::Proc(key, path.cloned()));
            self.cv.notify_one();
        }
        None
    }

    /// Icon for a file path or "path,index" resource spec. None while loading or if there is none.
    pub fn icon(&self, spec: &str) -> Option<Rgba> {
        if spec.is_empty() {
            return None;
        }
        let mut st = self.st.lock().unwrap();
        if let Some(i) = st.icons.get(spec) {
            return i.clone();
        }
        if st.pending_icons.insert(spec.to_string()) {
            st.queue.push_back(Job::Icon(spec.to_string()));
            self.cv.notify_one();
        }
        None
    }

    /// Drop entries for processes that have exited.
    pub fn prune(&self, live: &HashSet<Key>) {
        let mut st = self.st.lock().unwrap();
        st.by_key.retain(|k, _| live.contains(k));
    }

    /// Re-read the command line etc. (e.g. after elevation).
    pub fn forget(&self, key: Key) {
        self.st.lock().unwrap().by_key.remove(&key);
    }
}
