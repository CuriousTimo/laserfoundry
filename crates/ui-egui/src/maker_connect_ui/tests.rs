#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;

use laserfoundry_ai::maker_connect::HubRequest;
use laserfoundry_device::Transport;
use laserfoundry_engine::Session;
use serde_json::json;

use super::*;
use crate::Platform;

const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="40mm" height="20mm" viewBox="0 0 40 20"><rect width="40" height="20" fill="none" stroke="#ff0000"/></svg>"##;
const LASERS: &str = r#"[{"id":"8c1f","name":"Ortur","kind":"laser","model":"Ortur Laser Master 2","online":true,"work_area_mm":[400.0,430.0],"laser":{"power_w":10.0,"max_s":1000.0,"laser_mode":true},"may":"use"}]"#;

mod send_tests {
    use super::*;

    #[test]
    fn the_drawings_own_gcode_is_kept_and_then_loaded_on_the_chosen_laser() {
        let (mut app, shared) = app_with_lasers();
        let expected = app.run("job.gcode", json!({ "include": true })).unwrap()["gcode"].as_str().unwrap().to_string();
        app.send_to(app.hub.lasers[0].clone());
        let sent = shared.borrow().sent.clone();
        assert_eq!(sent.len(), 2, "the lasers, then the file");
        let upload = &sent[1];
        assert_eq!((upload.method, upload.path.as_str(), upload.content_type), ("POST", "/api/files?name=design.gcode", Some("text/plain")));
        assert_eq!(upload.body, expected, "exactly what Save G-code writes: nothing added");
        answer(&mut app, &shared, 201, r#"{"id":"f3a9"}"#);
        let load = shared.borrow().sent.last().unwrap().clone();
        assert_eq!((load.path.as_str(), load.body.as_str()), ("/files/f3a9/print", "machine=8c1f"));
        answer(&mut app, &shared, 200, r#"{"loaded":"design.gcode"}"#);
        assert_eq!(app.hub.note, Some(("design.gcode is loaded on Ortur. Frame it and start it at the laser.".to_string(), false)));
        assert_eq!(shared.borrow().sent.len(), 3, "nothing else is sent: no start, no queue");
    }

    #[test]
    fn a_loaded_job_is_replaced_only_after_the_person_agrees() {
        let (mut app, shared) = app_with_lasers();
        app.send_to(app.hub.lasers[0].clone());
        answer(&mut app, &shared, 201, r#"{"id":"f3a9"}"#);
        answer(&mut app, &shared, 409, r#"{"loaded":"old.gcode","confirm":"old.gcode is loaded on this laser: replace it with design.gcode?"}"#);
        assert!(matches!(app.hub.step, Step::Replacing { .. }));
        let before = shared.borrow().sent.len();
        poll(&mut app);
        assert_eq!(shared.borrow().sent.len(), before, "the question waits; nothing is resent by itself");
        app.load_on_replace_for_test();
        let again = shared.borrow().sent.last().unwrap().clone();
        assert_eq!((again.path.as_str(), again.body.as_str()), ("/files/f3a9/print", "machine=8c1f&replace=true"));
        answer(&mut app, &shared, 200, r#"{"loaded":"design.gcode"}"#);
        assert!(!app.hub.note.clone().unwrap().1);
    }

    #[test]
    fn a_refusal_is_shown_in_the_hubs_words_and_nothing_is_retried() {
        let (mut app, shared) = app_with_lasers();
        app.send_to(app.hub.lasers[0].clone());
        answer(&mut app, &shared, 201, r#"{"id":"f3a9"}"#);
        answer(&mut app, &shared, 409, "laser mode is off");
        assert_eq!(app.hub.note, Some(("laser mode is off".to_string(), true)));
        assert!(matches!(app.hub.step, Step::Idle));
        assert_eq!(shared.borrow().sent.len(), 3);
    }

    #[test]
    fn an_empty_drawing_sends_nothing() {
        let (app, shared) = app_with_lasers();
        let mut empty = fresh(&shared);
        empty.hub.lasers = app.hub.lasers.clone();
        empty.send_to(empty.hub.lasers[0].clone());
        assert_eq!(shared.borrow().sent.len(), 1, "only the lasers were asked for");
        assert!(empty.hub.note.unwrap().1);
    }
}

impl LaserFoundryApp {
    /// What pressing "Replace it on …" does.
    fn load_on_replace_for_test(&mut self) {
        if let Step::Replacing { laser, file, .. } = std::mem::take(&mut self.hub.step) {
            self.load_on(laser, file, true);
        }
    }
}

#[derive(Default)]
struct Shared {
    sent: Vec<HubRequest>,
    answers: VecDeque<Result<(u16, String), String>>,
    in_flight: bool,
}

struct Fake(Rc<RefCell<Shared>>);

impl Platform for Fake {
    fn pick_open(&self, _: &str, _: &[&str]) -> Option<PathBuf> {
        None
    }

    fn pick_save(&self, _: &str, _: &str, _: &[&str]) -> Option<PathBuf> {
        None
    }

    fn serial_ports(&self) -> Vec<String> {
        Vec::new()
    }

    fn open_serial(&self, _: &str) -> Result<Box<dyn Transport + Send>, String> {
        Err("no serial".into())
    }

    fn hub_available(&self) -> bool {
        true
    }

    fn start_hub(&self, request: HubRequest) -> bool {
        let mut s = self.0.borrow_mut();
        if s.in_flight {
            return false;
        }
        s.in_flight = true;
        s.sent.push(request);
        true
    }

    fn take_hub(&self) -> Option<Result<(u16, String), String>> {
        let mut s = self.0.borrow_mut();
        let answer = s.answers.pop_front()?;
        s.in_flight = false;
        Some(answer)
    }
}

fn fresh(shared: &Rc<RefCell<Shared>>) -> LaserFoundryApp {
    LaserFoundryApp::new(Session::new(), Box::new(Fake(Rc::clone(shared))))
}

/// An app with a drawing, whose window was opened and whose lasers came back (one request so far).
fn app_with_lasers() -> (LaserFoundryApp, Rc<RefCell<Shared>>) {
    let shared = Rc::new(RefCell::new(Shared::default()));
    let mut app = fresh(&shared);
    app.run("file.import_svg", json!({ "data": SVG })).unwrap();
    app.open_hub_window();
    assert_eq!(shared.borrow().sent[0].path, "/api/laser-design/lasers");
    answer(&mut app, &shared, 200, LASERS);
    assert_eq!(app.hub.lasers.len(), 1);
    (app, shared)
}

fn poll(app: &mut LaserFoundryApp) {
    app.poll_hub(&egui::Context::default());
}

fn answer(app: &mut LaserFoundryApp, shared: &Rc<RefCell<Shared>>, status: u16, body: &str) {
    shared.borrow_mut().answers.push_back(Ok((status, body.to_string())));
    poll(app);
}
