//! The media pool: one shared [`MediaSource`] per project item, created lazily from its
//! [`MediaRef`], plus the registry of container/codec openers.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{MediaError, OfflineSource, Opener, SharedSource};
use filmcraft_project::{ItemId, ItemKind, MediaRef, Project};

use crate::Services;

pub struct MediaPool {
    sources: RwLock<HashMap<ItemId, SharedSource>>,
    /// Openers tried before the built-in ones (MP4/MOV + codecs register here).
    pub openers: RwLock<Vec<Opener>>,
}

impl Default for MediaPool {
    /// A pool with the built-in container/codec openers registered.
    fn default() -> Self {
        Self { sources: RwLock::new(HashMap::new()), openers: RwLock::new(filmcraft_codecs::openers()) }
    }
}

impl MediaPool {
    pub fn register_opener(&self, o: Opener) {
        self.openers.write().unwrap_or_else(|e| e.into_inner()).push(o);
    }

    pub fn insert(&self, item: ItemId, src: SharedSource) {
        self.sources.write().unwrap_or_else(|e| e.into_inner()).insert(item, src);
    }

    pub fn remove(&self, item: ItemId) {
        self.sources.write().unwrap_or_else(|e| e.into_inner()).remove(&item);
    }

    pub fn cached(&self, item: ItemId) -> Option<SharedSource> {
        self.sources.read().unwrap_or_else(|e| e.into_inner()).get(&item).cloned()
    }

    /// Open a file through the registered openers.
    pub fn open_bytes(&self, name: &str, bytes: Arc<[u8]>) -> Result<SharedSource, MediaError> {
        let openers = self.openers.read().unwrap_or_else(|e| e.into_inner()).clone();
        filmcraft_media::open_bytes(name, bytes, &openers)
    }

    /// Resolve (and cache) the source for a project item.
    pub fn source_for(&self, p: &Project, item: ItemId, services: &dyn Services) -> Option<SharedSource> {
        if let Some(s) = self.cached(item) {
            return Some(s);
        }
        let it = p.item(item)?;
        let src: SharedSource = match &it.kind {
            ItemKind::Media(m) => match &m.media {
                MediaRef::Generator(g) => {
                    let v = m.info.video.as_ref();
                    let (w, h, r) = v.map(|v| (v.width, v.height, v.frame_rate)).unwrap_or((1920, 1080, Default::default()));
                    Arc::new(GeneratorSource::new(g.clone(), w, h, r, m.info.duration).with_name(&it.name))
                }
                MediaRef::File { path } => {
                    let opened = services.read_file(path).map_err(|e| MediaError::Io(e.to_string())).and_then(|b| {
                        let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                        self.open_bytes(&name, b.into())
                    });
                    match opened {
                        Ok(s) => s,
                        Err(e) => {
                            log::warn!("media offline: {path}: {e}");
                            Arc::new(OfflineSource { info: m.info.clone() })
                        }
                    }
                }
            },
            ItemKind::Subclip { parent, .. } => return self.source_for(p, *parent, services),
            _ => return None,
        };
        self.insert(item, src.clone());
        Some(src)
    }

    /// A render-side provider bound to a project snapshot.
    pub fn provider(self: &Arc<Self>, project: Arc<Project>, services: Arc<dyn Services>) -> PoolProvider {
        PoolProvider { pool: self.clone(), project, services }
    }
}

/// Implements [`filmcraft_render::SourceProvider`] over the pool for one project snapshot.
pub struct PoolProvider {
    pub pool: Arc<MediaPool>,
    pub project: Arc<Project>,
    pub services: Arc<dyn Services>,
}

impl filmcraft_render::SourceProvider for PoolProvider {
    fn source(&self, item: ItemId) -> Option<SharedSource> {
        self.pool.source_for(&self.project, item, &*self.services)
    }
}
