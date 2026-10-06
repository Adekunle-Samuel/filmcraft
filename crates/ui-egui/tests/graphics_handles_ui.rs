//! Headless UI tests of handle scaling of graphic layers in the Program monitor.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
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
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000).build_eframe(move |_cc| app);
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
}

impl Driver {
    fn rect(&mut self, id: &str) -> [f64; 4] {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == json!(id)).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r = &e["rect"];
        [r[0].as_f64().unwrap(), r[1].as_f64().unwrap(), r[2].as_f64().unwrap(), r[3].as_f64().unwrap()]
    }
}

impl Driver {
    /// A selected text layer; returns the id prefix of its handles (0–3 corners TL, TR, BR, BL;
    /// 4–7 the top, right, bottom and left edges).
    fn text_layer(&mut self) -> String {
        let r = self.exec("graphics.newText", json!({"text": "Title", "position": [600, 500], "size": 120}));
        let clip = r["clip"].as_u64().unwrap();
        self.exec("graphics.selectLayer", json!({"clip": clip, "layers": [0]}));
        self.ok("ui.set", json!({"tool": "Selection"}));
        self.frames(4);
        format!("program.layer.{clip}.0.handle.")
    }

    /// Centre of handle `n`.
    fn handle(&mut self, pre: &str, n: usize) -> (f64, f64) {
        let r = self.rect(&format!("{pre}{n}"));
        (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0)
    }

    fn drag_handle(&mut self, pre: &str, n: usize, dx: f64, dy: f64) {
        let (x, y) = self.handle(pre, n);
        self.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x + dx, "y": y + dy}}));
        self.frames(4);
    }
}

fn near(a: (f64, f64), b: (f64, f64)) -> bool {
    (a.0 - b.0).abs() < 1.5 && (a.1 - b.1).abs() < 1.5
}

#[test]
fn corner_handles_scale_away_from_the_opposite_corner() {
    let mut d = Driver::new();
    let pre = d.text_layer();
    // bottom-right: grows down and to the right, the top-left stays
    let (tl, br) = (d.handle(&pre, 0), d.handle(&pre, 2));
    d.drag_handle(&pre, 2, 60.0, 30.0);
    let (tl2, br2) = (d.handle(&pre, 0), d.handle(&pre, 2));
    assert!(near(tl, tl2), "top-left stays put: {tl:?} -> {tl2:?}");
    assert!(br2.0 > br.0 + 20.0 && br2.1 > br.1 + 5.0, "bottom-right follows the pointer: {br:?} -> {br2:?}");
    // bottom-left: grows down and to the left, the top-right stays
    let (tr, bl) = (d.handle(&pre, 1), d.handle(&pre, 3));
    d.drag_handle(&pre, 3, -60.0, 30.0);
    let (tr2, bl2) = (d.handle(&pre, 1), d.handle(&pre, 3));
    assert!(near(tr, tr2), "top-right stays put: {tr:?} -> {tr2:?}");
    assert!(bl2.0 < bl.0 - 20.0 && bl2.1 > bl.1 + 5.0, "bottom-left follows the pointer: {bl:?} -> {bl2:?}");
}

#[test]
fn edge_handles_stretch_one_axis() {
    let mut d = Driver::new();
    let pre = d.text_layer();
    // bottom edge: taller downwards, the top edge and the width stay
    let (tl, tr, bottom) = (d.handle(&pre, 0), d.handle(&pre, 1), d.handle(&pre, 6));
    d.drag_handle(&pre, 6, 0.0, 40.0);
    let (tl2, tr2, bottom2) = (d.handle(&pre, 0), d.handle(&pre, 1), d.handle(&pre, 6));
    assert!(near(tl, tl2) && near(tr, tr2), "top edge stays put: {tl:?} {tr:?} -> {tl2:?} {tr2:?}");
    assert!((bottom2.1 - bottom.1 - 40.0).abs() < 2.0 && (bottom2.0 - bottom.0).abs() < 1.5, "bottom follows the pointer: {bottom:?} -> {bottom2:?}");
    // right edge: wider to the right, the left edge and the height stay
    let (tl, bl, right) = (d.handle(&pre, 0), d.handle(&pre, 3), d.handle(&pre, 5));
    d.drag_handle(&pre, 5, 50.0, 0.0);
    let (tl2, bl2, right2) = (d.handle(&pre, 0), d.handle(&pre, 3), d.handle(&pre, 5));
    assert!(near(tl, tl2) && near(bl, bl2), "left edge stays put: {tl:?} {bl:?} -> {tl2:?} {bl2:?}");
    assert!((right2.0 - right.0 - 50.0).abs() < 2.0 && (right2.1 - right.1).abs() < 1.5, "right follows the pointer: {right:?} -> {right2:?}");
    // top edge: taller upwards, the bottom edge stays
    let (bl, top) = (d.handle(&pre, 3), d.handle(&pre, 4));
    d.drag_handle(&pre, 4, 0.0, -30.0);
    let (bl2, top2) = (d.handle(&pre, 3), d.handle(&pre, 4));
    assert!(near(bl, bl2), "bottom edge stays put: {bl:?} -> {bl2:?}");
    assert!((top2.1 - top.1 + 30.0).abs() < 2.0, "top follows the pointer: {top:?} -> {top2:?}");
}
