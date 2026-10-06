//! Graphic (title) clips and adjustment layers have no equivalent in the interchange formats. An
//! export used to lose them without a word (FCP7 XML, OTIO, EDL) or with a line that named
//! nothing (FCPXML, AAF): every such clip is now named in the export report.

mod support;

use filmcraft_interchange::{ExportOptions, Format, Report, export, import};
use filmcraft_project::{ItemId, ItemKind, Label, ParamValue, Project, TrackKind, find_effect};
use filmcraft_time::FrameRate;
use support::*;

const RATE: FrameRate = FrameRate::FPS_24;

/// V1: a media clip (frames 0–48) and an adjustment layer (48–72); V2: a title (12–36).
fn project_with_a_title() -> (Project, ItemId) {
    let mut p = Project::new("Titles");
    let s = sequence(&mut p, "Cut", RATE, false);
    let a = media(&mut p, "/m/a.mov", true, true, RATE);
    let graphic = p.add_item("Graphic", Label::Rose, ItemKind::Graphic { width: 1920, height: 1080, rate: RATE }, None);
    let layer =
        p.add_item("Adjustment Layer", Label::Iris, ItemKind::AdjustmentLayer { width: 1920, height: 1080, rate: RATE, duration: RATE.tick_of(240) }, None);
    clip(&mut p, s, TrackKind::Video, 0, a, 0, 48, 0);
    let adj = clip(&mut p, s, TrackKind::Video, 0, layer, 48, 24, 0);
    let title = clip(&mut p, s, TrackKind::Video, 1, graphic, 12, 24, 0);
    let q = p.sequence_mut(s).unwrap();
    let (_, t) = q.find_item_mut(title).unwrap();
    t.name = "Opening \"line\"".into();
    let mut text = find_effect("graphic_text").unwrap().instance();
    text.params.get_mut("text").unwrap().value = ParamValue::Text("Opening line".into());
    t.effects.push(text);
    q.find_item_mut(adj).unwrap().1.name = "Grade".into();
    q.check().unwrap();
    (p, s)
}

fn export_report(p: &Project, s: ItemId, format: Format) -> (Vec<u8>, Report) {
    export(p, s, format, &ExportOptions::default()).unwrap_or_else(|e| panic!("{format:?}: {e}"))
}

const TITLE: &str = "graphic clip \"Opening \"line\"\" at frame 12";
const LAYER: &str = "adjustment layer \"Grade\" at frame 48";

#[test]
fn every_format_names_the_title_and_the_adjustment_layer_it_cannot_carry() {
    let (p, s) = project_with_a_title();
    for format in [Format::Fcp7Xml, Format::Fcpxml, Format::Otio, Format::Aaf] {
        let (_, report) = export_report(&p, s, format);
        for clip in [TITLE, LAYER] {
            let named: Vec<_> = report.warnings().filter(|e| e.message.starts_with(clip)).collect();
            assert_eq!(named.len(), 1, "{format:?} names {clip} once, as a warning: {report}");
            assert_eq!(named[0].count, 1);
        }
    }
    // an EDL holds one video track: V1 by default (the layer), V2 when asked (the title)
    let (_, report) = export_report(&p, s, Format::Edl);
    assert!(report.mentions(LAYER) && !report.mentions(TITLE), "{report}");
    let mut opts = ExportOptions::default();
    opts.edl.video_track = Some(1);
    let (_, report) = export(&p, s, Format::Edl, &opts).unwrap();
    assert!(report.mentions(TITLE), "{report}");
}

#[test]
fn two_titles_are_two_report_lines() {
    let (mut p, s) = project_with_a_title();
    let graphic = p.items.values().find(|i| matches!(i.kind, ItemKind::Graphic { .. })).map(|i| i.id).unwrap();
    let second = clip(&mut p, s, TrackKind::Video, 1, graphic, 40, 10, 0);
    p.sequence_mut(s).unwrap().find_item_mut(second).unwrap().1.name = "Closing".into();
    for format in [Format::Fcp7Xml, Format::Fcpxml, Format::Otio, Format::Aaf] {
        let (_, report) = export_report(&p, s, format);
        assert!(report.mentions(TITLE) && report.mentions("graphic clip \"Closing\" at frame 40"), "{format:?}: {report}");
    }
}

#[test]
fn a_sequence_without_such_clips_reports_nothing_about_them() {
    let mut p = Project::new("Plain");
    let s = sequence(&mut p, "Cut", RATE, false);
    let a = media(&mut p, "/m/a.mov", true, true, RATE);
    clip(&mut p, s, TrackKind::Video, 0, a, 0, 48, 0);
    for format in [Format::Fcp7Xml, Format::Fcpxml, Format::Otio, Format::Edl, Format::Aaf] {
        let (_, report) = export_report(&p, s, format);
        assert!(!report.mentions("graphic clip") && !report.mentions("adjustment layer"), "{format:?}: {report}");
    }
}

/// What the report says happens is what happens: the written documents do not bring the title
/// back as a graphic.
#[test]
fn the_reports_match_what_an_import_gives_back() {
    let (p, s) = project_with_a_title();
    let graphics = |q: &Project| {
        q.sequences()
            .filter_map(|i| i.as_sequence())
            .flat_map(|q| q.all_tracks())
            .flat_map(|t| &t.items)
            .filter(|c| matches!(q.item(c.item).map(|i| &i.kind), Some(ItemKind::Graphic { .. })))
            .count()
    };
    assert_eq!(graphics(&p), 1);
    for format in [Format::Fcp7Xml, Format::Fcpxml, Format::Otio] {
        let (bytes, _) = export_report(&p, s, format);
        let (imported, _) = import(&bytes, format, None).unwrap_or_else(|e| panic!("{format:?}: {e}"));
        assert_eq!(graphics(&imported.project), 0, "{format:?}");
        let seq = only_seq(&imported);
        let names: Vec<String> = imported.project.sequence(seq).unwrap().video_tracks.iter().flat_map(|t| &t.items).map(|c| c.name.clone()).collect();
        // "not read back" (the XML formats) / "read back as offline media" (OTIO)
        assert_eq!(names.iter().any(|n| n.starts_with("Opening")), format == Format::Otio, "{format:?}: {names:?}");
    }
}
