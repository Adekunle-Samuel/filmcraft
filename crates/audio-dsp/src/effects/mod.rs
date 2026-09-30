//! Audio effects and their registry.

use crate::{AudioEffect, ParamSpec};

/// Effect category (for menus / the Effects panel's Audio Effects bin).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    Filter,
    Dynamics,
    Time,
    Channel,
    Restoration,
    Pitch,
}

/// Registry entry describing one effect type.
#[derive(Clone, Copy)]
pub struct EffectInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub category: Category,
    pub params: &'static [ParamSpec],
    /// Construct an instance for `(sample_rate, channels)`.
    pub create: fn(f32, usize) -> Box<dyn AudioEffect>,
}

impl std::fmt::Debug for EffectInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EffectInfo")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("category", &self.category)
            .field("params", &self.params.len())
            .finish()
    }
}

static REGISTRY: &[EffectInfo] = &[];

/// All registered effects.
pub fn effects() -> &'static [EffectInfo] {
    REGISTRY
}

/// Look up an effect by id.
pub fn effect_info(id: &str) -> Option<&'static EffectInfo> {
    REGISTRY.iter().find(|e| e.id == id)
}

/// Instantiate an effect by id.
pub fn create_effect(id: &str, sample_rate: f32, channels: usize) -> Option<Box<dyn AudioEffect>> {
    effect_info(id).map(|e| (e.create)(sample_rate, channels))
}
