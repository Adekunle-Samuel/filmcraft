//! Headless UI test of corner-handle scaling of graphic layers in the Program monitor.

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

#[test]
fn corner_handle_scales_away_from_the_opposite_corner() {
    let mut d = Driver::new();
    let r = d.exec("graphics.newText", json!({"text": "Title", "position": [600, 500], "size": 120}));
    let clip = r["clip"].as_u64().unwrap();
    d.exec("graphics.selectLayer", json!({"clip": clip, "layers": [0]}));
    d.ok("ui.set", json!({"tool": "Selection"}));
    d.frames(4);
    let pre = format!("program.layer.{clip}.0.handle.");
    let (tl, br) = (d.rect(&format!("{pre}0")), d.rect(&format!("{pre}2")));
    let (bx, by) = (br[0] + br[2] / 2.0, br[1] + br[3] / 2.0);
    d.ok("ui.drag", json!({"from": {"x": bx, "y": by}, "to": {"x": bx + 60.0, "y": by + 30.0}}));
    d.frames(4);
    let (tl2, br2) = (d.rect(&format!("{pre}0")), d.rect(&format!("{pre}2")));
    assert!((tl2[0] - tl[0]).abs() < 1.5 && (tl2[1] - tl[1]).abs() < 1.5, "top-left stays put: {tl:?} -> {tl2:?}");
    assert!(br2[0] > br[0] + 20.0 && br2[1] > br[1] + 5.0, "bottom-right follows the pointer down and right: {br:?} -> {br2:?}");
}
