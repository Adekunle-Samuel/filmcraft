//! Background frame rendering for monitors, thumbnails and playback prefetch.
//!
//! A small pool of worker threads pulls prioritised jobs (the frame on screen first, then the
//! frames playback will need next, then thumbnails). Results land in a byte-budgeted cache keyed
//! by (target, frame, scale, revision); the UI shows the exact frame when ready and otherwise holds
//! the nearest frame it already has, so scrubbing never flashes black and never blocks the UI.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};

use filmcraft_engine::{MediaPool, Services};
use filmcraft_project::{ItemId, Project};
use filmcraft_time::Tick;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// Composite of a sequence at a timeline time.
    Sequence(ItemId),
    /// A single project item at a media time (Source monitor, thumbnails).
    Item(ItemId),
    /// A GPU frame plan of a sequence (decoded layers + transforms), composited on the GPU.
    SequencePlan(ItemId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameKey {
    pub target: Target,
    pub frame: i64,
    /// Output width in pixels (thumbnails) or scale ×1000 (monitors).
    pub size: u32,
    pub revision: u64,
}

pub struct Rgba {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

struct Job {
    key: FrameKey,
    time: Tick,
    /// Output scale relative to the target's frame size.
    scale: f32,
    project: Arc<Project>,
    prio: u32,
}

struct Shared {
    queue: Mutex<VecDeque<Job>>,
    cv: Condvar,
    done: Mutex<Cache>,
    plans: Mutex<HashMap<FrameKey, (Arc<filmcraft_render::plan::FramePlan>, u64)>>,
    in_flight: Mutex<Vec<FrameKey>>,
}

struct Cache {
    map: HashMap<FrameKey, (Arc<Rgba>, u64)>,
    bytes: usize,
    budget: usize,
    clock: u64,
}

impl Cache {
    fn insert(&mut self, k: FrameKey, v: Arc<Rgba>) {
        self.clock += 1;
        self.bytes += v.px.len();
        if let Some((old, _)) = self.map.insert(k, (v, self.clock)) {
            self.bytes -= old.px.len();
        }
        if self.bytes > self.budget {
            let mut all: Vec<(u64, FrameKey, usize)> = self.map.iter().map(|(k, (v, s))| (*s, *k, v.px.len())).collect();
            all.sort_unstable_by_key(|x| x.0);
            for (_, k, b) in all {
                if self.bytes <= self.budget * 8 / 10 {
                    break;
                }
                self.map.remove(&k);
                self.bytes -= b;
            }
        }
    }
}

pub struct FrameServer {
    shared: Arc<Shared>,
    pub pool: Arc<MediaPool>,
    pub services: Arc<dyn Services>,
    repaint: Arc<Mutex<Option<egui::Context>>>,
}

impl FrameServer {
    pub fn new(pool: Arc<MediaPool>, services: Arc<dyn Services>, workers: usize) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(VecDeque::new()),
            cv: Condvar::new(),
            done: Mutex::new(Cache { map: HashMap::new(), bytes: 0, budget: 768 << 20, clock: 0 }),
            plans: Mutex::new(HashMap::new()),
            in_flight: Mutex::new(Vec::new()),
        });
        let repaint: Arc<Mutex<Option<egui::Context>>> = Arc::new(Mutex::new(None));
        #[cfg(not(target_arch = "wasm32"))]
        for i in 0..workers.max(1) {
            let sh = shared.clone();
            let pool = pool.clone();
            let services = services.clone();
            let rp = repaint.clone();
            std::thread::Builder::new().name(format!("filmcraft-frames-{i}")).spawn(move || worker(sh, pool, services, rp)).ok();
        }
        #[cfg(target_arch = "wasm32")]
        let _ = workers;
        Self { shared, pool, services, repaint }
    }

    pub fn set_context(&self, ctx: &egui::Context) {
        let mut g = self.repaint.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_none() {
            *g = Some(ctx.clone());
        }
    }

    pub fn get(&self, k: &FrameKey) -> Option<Arc<Rgba>> {
        let mut c = self.shared.done.lock().unwrap_or_else(|e| e.into_inner());
        c.clock += 1;
        let clock = c.clock;
        c.map.get_mut(k).map(|(v, s)| {
            *s = clock;
            v.clone()
        })
    }

    pub fn get_plan(&self, k: &FrameKey) -> Option<Arc<filmcraft_render::plan::FramePlan>> {
        self.shared.plans.lock().unwrap_or_else(|e| e.into_inner()).get(k).map(|(p, _)| p.clone())
    }

    /// Nearest cached plan at or before `frame`.
    pub fn nearest_plan(&self, key: FrameKey, max_back: i64) -> Option<(FrameKey, Arc<filmcraft_render::plan::FramePlan>)> {
        let g = self.shared.plans.lock().unwrap_or_else(|e| e.into_inner());
        (0..=max_back).find_map(|d| {
            let k = FrameKey { frame: key.frame - d, ..key };
            g.get(&k).map(|(p, _)| (k, p.clone()))
        })
    }

    /// Queue a job unless it is cached, queued or in flight.
    pub fn request(&self, key: FrameKey, time: Tick, scale: f32, project: &Arc<Project>, prio: u32) {
        if self.shared.done.lock().unwrap_or_else(|e| e.into_inner()).map.contains_key(&key)
            || self.shared.plans.lock().unwrap_or_else(|e| e.into_inner()).contains_key(&key)
        {
            return;
        }
        if self.shared.in_flight.lock().unwrap_or_else(|e| e.into_inner()).contains(&key) {
            return;
        }
        let mut q = self.shared.queue.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(j) = q.iter_mut().find(|j| j.key == key) {
            j.prio = j.prio.min(prio);
            return;
        }
        q.push_back(Job { key, time, scale, project: project.clone(), prio });
        drop(q);
        self.shared.cv.notify_one();
        #[cfg(target_arch = "wasm32")]
        self.run_one_sync();
    }

    /// Drop queued jobs that fail `keep` (e.g. stale revisions or frames far from the playhead).
    pub fn retain_queue(&self, keep: impl Fn(&FrameKey) -> bool) {
        self.shared.queue.lock().unwrap_or_else(|e| e.into_inner()).retain(|j| keep(&j.key));
    }

    pub fn queue_len(&self) -> usize {
        self.shared.queue.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// The most recent cached frame for `target` at or before `frame` within `max_back` frames.
    pub fn nearest(&self, target: Target, frame: i64, size: u32, revision: u64, max_back: i64) -> Option<Arc<Rgba>> {
        for d in 0..=max_back {
            if let Some(v) = self.get(&FrameKey { target, frame: frame - d, size, revision }) {
                return Some(v);
            }
        }
        None
    }

    #[cfg(target_arch = "wasm32")]
    fn run_one_sync(&self) {
        let job = { self.shared.queue.lock().unwrap_or_else(|e| e.into_inner()).pop_front() };
        if let Some(job) = job {
            if let Target::SequencePlan(seq) = job.key.target {
                let provider = self.pool.provider(job.project.clone(), self.services.clone());
                let plan = filmcraft_render::plan::plan_frame(
                    &job.project,
                    seq,
                    job.time,
                    filmcraft_render::RenderOptions { scale: job.scale, ..Default::default() },
                    &provider,
                );
                self.shared.plans.lock().unwrap_or_else(|e| e.into_inner()).insert(job.key, (Arc::new(plan), 0));
            } else {
                let img = render_job(&job, &self.pool, &self.services);
                self.shared.done.lock().unwrap_or_else(|e| e.into_inner()).insert(job.key, Arc::new(img));
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn worker(sh: Arc<Shared>, pool: Arc<MediaPool>, services: Arc<dyn Services>, repaint: Arc<Mutex<Option<egui::Context>>>) {
    loop {
        let job = {
            let mut q = sh.queue.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if !q.is_empty() {
                    // highest priority (lowest number) first; FIFO among equals
                    let best = q.iter().enumerate().min_by_key(|(i, j)| (j.prio, *i)).map(|(i, _)| i).unwrap_or(0);
                    break q.remove(best).expect("index valid");
                }
                q = sh.cv.wait(q).unwrap_or_else(|e| e.into_inner());
            }
        };
        sh.in_flight.lock().unwrap_or_else(|e| e.into_inner()).push(job.key);
        if let Target::SequencePlan(seq) = job.key.target {
            let provider = pool.provider(job.project.clone(), services.clone());
            let plan = filmcraft_render::plan::plan_frame(
                &job.project,
                seq,
                job.time,
                filmcraft_render::RenderOptions { scale: job.scale, ..Default::default() },
                &provider,
            );
            let mut g = sh.plans.lock().unwrap_or_else(|e| e.into_inner());
            let clock = g.values().map(|v| v.1).max().unwrap_or(0) + 1;
            g.insert(job.key, (Arc::new(plan), clock));
            if g.len() > 96 {
                let mut v: Vec<(u64, FrameKey)> = g.iter().map(|(k, v)| (v.1, *k)).collect();
                v.sort_unstable_by_key(|x| x.0);
                for (_, k) in v.into_iter().take(g.len() - 96) {
                    g.remove(&k);
                }
            }
        } else {
            let img = render_job(&job, &pool, &services);
            sh.done.lock().unwrap_or_else(|e| e.into_inner()).insert(job.key, Arc::new(img));
        }
        sh.in_flight.lock().unwrap_or_else(|e| e.into_inner()).retain(|k| *k != job.key);
        if let Some(ctx) = repaint.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            ctx.request_repaint();
        }
    }
}

fn render_job(job: &Job, pool: &Arc<MediaPool>, services: &Arc<dyn Services>) -> Rgba {
    let provider = pool.provider(job.project.clone(), services.clone());
    let img = match job.key.target {
        Target::Sequence(seq) => Some(filmcraft_render::render_sequence(
            &job.project,
            seq,
            job.time,
            filmcraft_render::RenderOptions { scale: job.scale, ..Default::default() },
            &provider,
        )),
        Target::Item(item) => filmcraft_render::render_item(&job.project, item, job.time, job.scale, &provider),
        Target::SequencePlan(seq) => Some(filmcraft_render::render_sequence(
            &job.project,
            seq,
            job.time,
            filmcraft_render::RenderOptions { scale: job.scale, ..Default::default() },
            &provider,
        )),
    };
    match img {
        Some(img) => Rgba { w: img.w, h: img.h, px: img.over_black_rgba8() },
        None => Rgba { w: 1, h: 1, px: vec![0, 0, 0, 255] },
    }
}
