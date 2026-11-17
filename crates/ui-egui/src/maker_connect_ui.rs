//! Maker Connect: sending the drawing's G-code to a laser of the hub that served this page.
//!
//! Only when the page was served by a Maker Connect hub ([`Platform::hub_available`](crate::Platform::hub_available)): the
//! File menu then offers **Send to a laser…**. The window lists the lasers the signed-in person may use, one **Send to <laser>**
//! button each. Sending keeps the G-code (exactly what **Save G-code** writes) in the hub's Files and loads it on that laser.
//! Nothing starts: the hub only loads the job, and a person at the laser frames it and starts it there. A laser holds one
//! loaded job, so when one is there the hub's question is shown and the job is sent again with `replace` only when the person
//! agrees.
//!
//! The requests and the answers are in `laserfoundry_ai::maker_connect`; this is the window and the hand-offs to the host.

use laserfoundry_ai::maker_connect::{self as hub, HubRequest, Laser, Load};
use serde_json::json;

use crate::LaserFoundryApp;
use crate::i18n::{tr, trf};

/// What the window is waiting for.
#[derive(Default)]
enum Step {
    #[default]
    Idle,
    /// The list of lasers is being asked for.
    Listing,
    /// The G-code is being kept in the hub's Files.
    Uploading(Laser),
    /// The kept file is being loaded on the laser.
    Loading { laser: Laser, file: String },
    /// The laser has a job loaded: the hub's question waits for the person.
    Replacing { laser: Laser, file: String, question: String },
}

/// What the Maker Connect window remembers.
#[derive(Default)]
pub(crate) struct HubState {
    open: bool,
    lasers: Vec<Laser>,
    step: Step,
    note: Option<(String, bool)>,
    /// The laser the last design was loaded on: its page on the hub is linked (to frame and start it there).
    loaded_on: Option<Laser>,
}

impl LaserFoundryApp {
    /// Whether the File menu offers sending to a laser.
    pub(crate) fn hub_available(&self) -> bool {
        self.platform.hub_available()
    }

    pub(crate) fn open_hub_window(&mut self) {
        self.hub.open = true;
        self.hub.note = None;
        self.hub.loaded_on = None;
        self.ask_hub(Step::Listing, hub::lasers_request());
    }

    /// Sends `request` and waits for its answer in `step`; a host that is busy or cannot says so.
    fn ask_hub(&mut self, step: Step, request: HubRequest) {
        if self.platform.start_hub(request) {
            self.hub.step = step;
        } else {
            self.hub.step = Step::Idle;
            self.hub.note = Some((tr("The hub could not be asked just now: try again."), true));
        }
    }

    /// Follows the request in flight to its end. Call every frame.
    pub(crate) fn poll_hub(&mut self, ctx: &egui::Context) {
        if matches!(self.hub.step, Step::Idle | Step::Replacing { .. }) {
            return;
        }
        let Some(result) = self.platform.take_hub() else {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
            return;
        };
        let (status, body) = match result {
            Ok(answer) => answer,
            Err(e) => return self.hub_failed(e),
        };
        match std::mem::take(&mut self.hub.step) {
            Step::Listing => match hub::read_lasers(status, &body) {
                Ok(lasers) => self.hub.lasers = lasers,
                Err(e) => self.hub_failed(e),
            },
            Step::Uploading(laser) => match hub::read_upload(status, &body) {
                Ok(file) => self.load_on(laser, file, false),
                Err(e) => self.hub_failed(e),
            },
            Step::Loading { laser, file } => match hub::read_load(status, &body) {
                Ok(Load::Loaded(name)) => {
                    self.hub.note =
                        Some((trf!("{name} is loaded on {laser}. Frame it and start it at the laser.", name = name, laser = laser.name), false));
                    self.say(trf!("Sent to {laser}", laser = laser.name));
                    self.hub.loaded_on = Some(laser);
                }
                Ok(Load::Replace { question, .. }) => self.hub.step = Step::Replacing { laser, file, question },
                Err(e) => self.hub_failed(e),
            },
            // not waiting for an answer
            step @ (Step::Idle | Step::Replacing { .. }) => self.hub.step = step,
        }
    }

    fn hub_failed(&mut self, text: String) {
        self.hub.step = Step::Idle;
        self.hub.note = Some((text, true));
    }

    fn load_on(&mut self, laser: Laser, file: String, replace: bool) {
        match hub::load_request(&file, &laser.id, replace) {
            Ok(request) => self.ask_hub(Step::Loading { laser, file }, request),
            Err(e) => self.hub_failed(e),
        }
    }

    /// Keeps the drawing's G-code, as **Save G-code** writes it, in the hub's Files; loading it on `laser` follows the answer.
    fn send_to(&mut self, laser: Laser) {
        self.hub.note = None;
        self.hub.loaded_on = None;
        if self.session.doc.shapes.is_empty() {
            self.hub.note = Some((tr("Draw something first: there is nothing to send."), true));
            return;
        }
        let Some(exported) = self.run("job.gcode", json!({ "include": true })) else { return };
        let Some(gcode) = exported["gcode"].as_str() else { return };
        let stem = self.session.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let name = hub::gcode_name(&stem).unwrap_or_else(|| "design.gcode".to_string());
        match hub::upload_request(&name, gcode) {
            Ok(request) => self.ask_hub(Step::Uploading(laser), request),
            Err(e) => self.hub_failed(e),
        }
    }

    pub(crate) fn hub_window(&mut self, ctx: &egui::Context) {
        if !self.hub.open {
            return;
        }
        let mut open = true;
        let mut send = None;
        let mut refresh = false;
        let mut replace = None;
        let busy = !matches!(self.hub.step, Step::Idle | Step::Replacing { .. });
        egui::Window::new(tr("Send to a laser")).open(&mut open).collapsible(false).default_width(420.0).default_pos(egui::pos2(260.0, 70.0)).show(
            ctx,
            |ui| {
                ui.label(tr("The drawing's G-code is loaded on the laser you choose. Nothing starts: frame it and start it at the laser."));
                if self.hub.lasers.is_empty() && !busy {
                    ui.weak(tr("No laser here that you may use."));
                }
                for laser in &self.hub.lasers {
                    ui.separator();
                    let detail =
                        [laser.model.clone(), laser.work_area_mm.map(|[x, y]| format!("{x:.0} × {y:.0} mm")), (!laser.online).then(|| tr("offline"))];
                    ui.weak(detail.into_iter().flatten().collect::<Vec<_>>().join(" · "));
                    if laser.laser_mode == Some(false) {
                        ui.colored_label(self.tokens.error, tr("Laser mode ($32) is off on this laser: the hub will refuse the job."));
                    }
                    if ui
                        .add_enabled(
                            !busy && !matches!(self.hub.step, Step::Replacing { .. }),
                            egui::Button::new(trf!("Send to {laser}", laser = laser.name)),
                        )
                        .clicked()
                    {
                        send = Some(laser.clone());
                    }
                }
                if let Step::Replacing { laser, question, .. } = &self.hub.step {
                    ui.separator();
                    ui.colored_label(self.tokens.error, question.as_str());
                    ui.horizontal(|ui| {
                        if ui.button(trf!("Replace it on {laser}", laser = laser.name)).clicked() {
                            replace = Some(true);
                        }
                        if ui.button(tr("Cancel")).clicked() {
                            replace = Some(false);
                        }
                    });
                }
                ui.horizontal(|ui| {
                    refresh = ui.add_enabled(!busy, egui::Button::new(tr("Refresh the lasers"))).clicked();
                    if busy {
                        ui.spinner();
                    }
                });
                if let Some((text, bad)) = &self.hub.note {
                    ui.colored_label(if *bad { self.tokens.error } else { self.tokens.ok }, text.as_str());
                    if let (false, Some(laser)) = (*bad, &self.hub.loaded_on) {
                        // The laser's page on the same hub, in a new tab so this design stays open.
                        let url = format!("/machines/{}#control", laser.id);
                        ui.add(egui::Hyperlink::from_label_and_url(trf!("Open {laser}'s page", laser = laser.name), url).open_in_new_tab(true));
                    }
                }
            },
        );
        if let Some(laser) = send {
            self.send_to(laser);
        }
        if refresh {
            self.ask_hub(Step::Listing, hub::lasers_request());
        }
        match replace {
            Some(true) => {
                if let Step::Replacing { laser, file, .. } = std::mem::take(&mut self.hub.step) {
                    self.load_on(laser, file, true);
                }
            }
            Some(false) => self.hub.step = Step::Idle,
            None => {}
        }
        if !open {
            self.hub.open = false;
            if matches!(self.hub.step, Step::Replacing { .. }) {
                self.hub.step = Step::Idle;
            }
        }
    }
}

#[cfg(test)]
mod tests;
