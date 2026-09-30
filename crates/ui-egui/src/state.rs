//! UI state that is not project data: tools, layout, zoom/scroll, monitor settings. Serde so the
//! control channel can read and set all of it.

use serde::{Deserialize, Serialize};

use crate::dock::{DockNode, PanelKind};
use crate::icons::Icon;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tool {
    #[default]
    Selection,
    TrackSelectForward,
    TrackSelectBackward,
    Ripple,
    Rolling,
    RateStretch,
    Razor,
    Slip,
    Slide,
    Pen,
    Rectangle,
    Ellipse,
    Hand,
    Zoom,
    Type,
}

impl Tool {
    pub const ALL: [Tool; 15] = [
        Tool::Selection,
        Tool::TrackSelectForward,
        Tool::TrackSelectBackward,
        Tool::Ripple,
        Tool::Rolling,
        Tool::RateStretch,
        Tool::Razor,
        Tool::Slip,
        Tool::Slide,
        Tool::Pen,
        Tool::Rectangle,
        Tool::Ellipse,
        Tool::Hand,
        Tool::Zoom,
        Tool::Type,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Tool::Selection => "Selection Tool",
            Tool::TrackSelectForward => "Track Select Forward Tool",
            Tool::TrackSelectBackward => "Track Select Backward Tool",
            Tool::Ripple => "Ripple Edit Tool",
            Tool::Rolling => "Rolling Edit Tool",
            Tool::RateStretch => "Rate Stretch Tool",
            Tool::Razor => "Razor Tool",
            Tool::Slip => "Slip Tool",
            Tool::Slide => "Slide Tool",
            Tool::Pen => "Pen Tool",
            Tool::Rectangle => "Rectangle Tool",
            Tool::Ellipse => "Ellipse Tool",
            Tool::Hand => "Hand Tool",
            Tool::Zoom => "Zoom Tool",
            Tool::Type => "Type Tool",
        }
    }
    pub fn shortcut(self) -> &'static str {
        match self {
            Tool::Selection => "V",
            Tool::TrackSelectForward => "A",
            Tool::TrackSelectBackward => "Shift+A",
            Tool::Ripple => "B",
            Tool::Rolling => "N",
            Tool::RateStretch => "R",
            Tool::Razor => "C",
            Tool::Slip => "Y",
            Tool::Slide => "U",
            Tool::Pen => "P",
            Tool::Rectangle | Tool::Ellipse => "",
            Tool::Hand => "H",
            Tool::Zoom => "Z",
            Tool::Type => "T",
        }
    }
    pub fn icon(self) -> Icon {
        match self {
            Tool::Selection => Icon::Selection,
            Tool::TrackSelectForward => Icon::TrackSelectFwd,
            Tool::TrackSelectBackward => Icon::TrackSelectBack,
            Tool::Ripple => Icon::Ripple,
            Tool::Rolling => Icon::Rolling,
            Tool::RateStretch => Icon::RateStretch,
            Tool::Razor => Icon::Razor,
            Tool::Slip => Icon::Slip,
            Tool::Slide => Icon::Slide,
            Tool::Pen => Icon::Pen,
            Tool::Rectangle => Icon::Rectangle,
            Tool::Ellipse => Icon::Ellipse,
            Tool::Hand => Icon::Hand,
            Tool::Zoom => Icon::Zoom,
            Tool::Type => Icon::Type,
        }
    }
    pub fn from_name(s: &str) -> Option<Tool> {
        let n = s.to_ascii_lowercase().replace([' ', '_', '-'], "").replace("tool", "");
        Tool::ALL
            .iter()
            .copied()
            .find(|t| format!("{t:?}").to_ascii_lowercase() == n || t.label().to_ascii_lowercase().replace([' ', '-'], "").replace("tool", "") == n)
    }
    /// Tools-panel groups (Premiere groups related tools under one button with a flyout).
    pub fn groups() -> Vec<Vec<Tool>> {
        vec![
            vec![Tool::Selection],
            vec![Tool::TrackSelectForward, Tool::TrackSelectBackward],
            vec![Tool::Ripple, Tool::Rolling, Tool::RateStretch],
            vec![Tool::Razor],
            vec![Tool::Slip, Tool::Slide],
            vec![Tool::Pen, Tool::Rectangle, Tool::Ellipse],
            vec![Tool::Hand, Tool::Zoom],
            vec![Tool::Type],
        ]
    }
}

/// Playback resolution (Premiere's Full / 1/2 / 1/4 / 1/8 / 1/16).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaybackRes {
    Full,
    #[default]
    Half,
    Quarter,
    Eighth,
    Sixteenth,
}

impl PlaybackRes {
    pub const ALL: [PlaybackRes; 5] = [PlaybackRes::Full, PlaybackRes::Half, PlaybackRes::Quarter, PlaybackRes::Eighth, PlaybackRes::Sixteenth];
    pub fn scale(self) -> f32 {
        match self {
            PlaybackRes::Full => 1.0,
            PlaybackRes::Half => 0.5,
            PlaybackRes::Quarter => 0.25,
            PlaybackRes::Eighth => 0.125,
            PlaybackRes::Sixteenth => 0.0625,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            PlaybackRes::Full => "Full",
            PlaybackRes::Half => "1/2",
            PlaybackRes::Quarter => "1/4",
            PlaybackRes::Eighth => "1/8",
            PlaybackRes::Sixteenth => "1/16",
        }
    }
}

/// Header mode (Import / Edit / Export).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    Import,
    #[default]
    Edit,
    Export,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TimelineView {
    /// Pixels per second (animated toward `target_pps`).
    pub pps: f64,
    pub target_pps: f64,
    /// Left edge time in seconds (animated toward `target_scroll`).
    pub scroll: f64,
    pub target_scroll: f64,
    /// Vertical scroll of the video / audio halves.
    pub v_scroll: f32,
    pub a_scroll: f32,
    /// Fraction of the track area given to video tracks.
    pub split: f32,
    pub video_track_h: f32,
    pub audio_track_h: f32,
    pub header_w: f32,
    pub show_thumbnails: bool,
    pub show_waveforms: bool,
    /// Follow playhead during playback (page scroll).
    pub follow: bool,
    pub fit_pending: bool,
}

impl Default for TimelineView {
    fn default() -> Self {
        Self {
            pps: 40.0,
            target_pps: 40.0,
            scroll: 0.0,
            target_scroll: 0.0,
            v_scroll: 0.0,
            a_scroll: 0.0,
            split: 0.5,
            video_track_h: 46.0,
            audio_track_h: 44.0,
            header_w: 196.0,
            show_thumbnails: true,
            show_waveforms: true,
            follow: true,
            fit_pending: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MonitorView {
    pub res: PlaybackRes,
    /// Zoom: None = Fit.
    pub zoom: Option<f32>,
    pub safe_margins: bool,
    pub show_transport: bool,
}

impl Default for MonitorView {
    fn default() -> Self {
        Self { res: PlaybackRes::Half, zoom: None, safe_margins: false, show_transport: true }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectView {
    #[default]
    List,
    Icon,
    Freeform,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UiState {
    pub tool: Tool,
    pub mode: Mode,
    pub workspace: String,
    pub dock: DockNode,
    /// Focused panel (blue outline, receives shortcuts).
    pub focused: PanelKind,
    pub timeline: TimelineView,
    pub program: MonitorView,
    pub source: MonitorView,
    pub project_view: ProjectView,
    pub project_search: String,
    pub effects_search: String,
    pub icon_size: f32,
    /// Expanded bins in the project list view.
    pub expanded_bins: Vec<u64>,
    /// Expanded folders in the Effects panel.
    pub expanded_fx: Vec<String>,
    /// Collapsed effect sections in Effect Controls ("clip:index").
    pub collapsed_fx: Vec<String>,
    pub show_menu_bar: bool,
    pub dark: bool,
    /// Lumetri scopes visible in the Program monitor area.
    pub show_scopes: bool,
    /// Transient status line shown in the footer.
    pub status: String,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            tool: Tool::Selection,
            mode: Mode::Edit,
            workspace: "Editing".into(),
            dock: crate::dock::workspace("Editing"),
            focused: PanelKind::Timeline,
            timeline: TimelineView::default(),
            program: MonitorView::default(),
            source: MonitorView::default(),
            project_view: ProjectView::List,
            project_search: String::new(),
            effects_search: String::new(),
            icon_size: 110.0,
            expanded_bins: vec![],
            expanded_fx: vec!["Video Transitions".into(), "Video Transitions/Dissolve".into()],
            collapsed_fx: vec![],
            show_menu_bar: true,
            dark: true,
            show_scopes: false,
            status: String::new(),
        }
    }
}
