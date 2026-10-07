//! Headless UI tests of the Timeline's sequence tabs: every sequence keeps its own zoom, scroll
//! and track heights. Premiere's behaviour was observed in Premiere Pro 26.5.2.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_engine::project::ItemId;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn new() -> Self {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(s).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            // the control channel's clicks and keys enter through the input hook
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let v = self.call(method, params.clone());
        assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
        v["result"].clone()
    }

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }
}

impl Driver {
    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(3);
    }

    fn new_sequence(&mut self, name: &str) -> u64 {
        let id = self.exec("file.newSequence", json!({"name": name}))["sequence"].as_u64().unwrap();
        self.frames(3);
        id
    }

    fn active(&mut self) -> u64 {
        self.app().session.state.active_sequence.unwrap().0
    }

    /// (zoom, scroll, video track height) the Timeline panel is showing.
    fn view(&mut self) -> (f64, f64, f32) {
        let v = &self.app().ui.timeline;
        (v.target_pps, v.target_scroll, v.video_track_h)
    }
}

#[test]
fn each_sequence_tab_keeps_its_own_zoom_scroll_and_track_heights() {
    let mut d = Driver::new();
    let main = d.active();
    d.ok("ui.set", json!({"timeline": {"pps": 333.0, "scroll": 4.5, "videoTrackHeight": 96.0}}));
    d.frames(3);
    assert_eq!(d.view(), (333.0, 4.5, 96.0));
    // a sequence shown for the first time is fitted, at the default track height
    let other = d.new_sequence("Other");
    let fitted = d.view();
    assert!(fitted.0 != 333.0 && fitted.1 == 0.0 && fitted.2 == 60.0, "{fitted:?}");
    d.ok("ui.set", json!({"timeline": {"pps": 20.0, "scroll": 1.0, "videoTrackHeight": 30.0}}));
    d.frames(3);
    // back and forth by clicking the tabs: each comes back as it was left
    d.click(&format!("timeline.tab.{main}"));
    assert_eq!(d.view(), (333.0, 4.5, 96.0));
    d.click(&format!("timeline.tab.{other}"));
    assert_eq!(d.view(), (20.0, 1.0, 30.0));
    // the session holds both (it is what a saved project keeps)
    let views = d.app().session.state.timeline_views.clone();
    assert_eq!((views[&ItemId(main)].pps, views[&ItemId(other)].pps), (333.0, 20.0));
    // closing the shown tab shows the other one with its view
    d.click(&format!("timeline.tab.{other}.close"));
    assert_eq!((d.active(), d.view()), (main, (333.0, 4.5, 96.0)));
    // a view changed in the session (a command, the control channel) is taken over by the panel
    d.app().session.state.timeline_views.get_mut(&ItemId(main)).unwrap().pps = 55.0;
    d.frames(2);
    assert_eq!(d.view().0, 55.0);
}
