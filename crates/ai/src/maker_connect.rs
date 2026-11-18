//! Maker Connect: sending the drawing's G-code to a laser of the Maker Connect hub that served this page.
//!
//! Maker Connect is the makerspace hub that lets people share machines. When the page was served by a hub, it is the same
//! origin as the page: a person who is signed in to the hub has a session cookie, and the page needs no token of its own.
//!
//! * **Lasers** ([`lasers_request`], [`read_lasers`]): the lasers of the signed-in person's space that they may use, with the
//!   work area and `$30`/`$32` as the machine's controller reported them (the hub's `apps` API gives the same fields).
//! * **Send** ([`upload_request`], [`read_upload`], [`load_request`], [`read_load`]): the G-code is kept in the hub's Files
//!   and then **loaded** on the chosen laser. Nothing starts: a person at the laser frames the job and presses Start there. A
//!   laser holds one loaded job, so when one is already loaded the hub asks first ([`Load::Replace`]) and the request is sent
//!   again with `replace` only after the person agrees.
//!
//! Like the rest of this crate, nothing here touches the network: it builds requests and reads answers.

use serde_json::Value;

/// Whether the page was served by a Maker Connect hub (an unauthenticated call every hub answers).
pub const INFO_PATH: &str = "/api/apps/v1/info";
/// The lasers the signed-in person may use.
pub const LASERS_PATH: &str = "/api/laser-design/lasers";
/// Where the hub keeps a file (the name and the file's text follow).
const FILES_PATH: &str = "/api/files";
/// The most G-code sent, in bytes (the hub takes more; this keeps one request a size a tab can build).
pub const MAX_GCODE_BYTES: usize = 64 * 1024 * 1024;
const MAX_NAME: usize = 120;

/// A request to the hub that served the page. The path starts with `/`: it is never sent anywhere but that origin.
#[derive(Clone, Debug, PartialEq)]
pub struct HubRequest {
    /// `GET` or `POST`.
    pub method: &'static str,
    pub path: String,
    pub content_type: Option<&'static str>,
    pub body: String,
}

/// A laser of the space that the person may use.
#[derive(Clone, Debug, PartialEq)]
pub struct Laser {
    pub id: String,
    pub name: String,
    /// The machine's make and model, when the hub knows it.
    pub model: Option<String>,
    pub online: bool,
    /// What the controller says it travels, in mm (the model's while the laser is offline).
    pub work_area_mm: Option<[f64; 2]>,
    /// `$32`: `Some(false)` when the beam would stay on while the head stops, which the hub refuses.
    pub laser_mode: Option<bool>,
    /// `$30`, as the controller reported it: `None` while the laser is offline (never read).
    pub max_s: Option<u32>,
}

/// What the hub did with a file sent for a laser.
#[derive(Clone, Debug, PartialEq)]
pub enum Load {
    /// The file is the laser's loaded job.
    Loaded(String),
    /// A job is already loaded: the hub's own question, to show before anything is replaced.
    Replace { loaded: String, question: String },
}

/// Whether the page may send `path` to the hub: exactly the calls above (a file name is the one thing that varies).
pub fn path_allowed(path: &str) -> bool {
    if matches!(path, INFO_PATH | LASERS_PATH) {
        return true;
    }
    if let Some(name) = path.strip_prefix("/api/files?name=") {
        return !name.is_empty()
            && name.len() <= MAX_NAME * 9
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'%' | b'-' | b'_' | b'.'));
    }
    path.strip_prefix("/files/")
        .and_then(|rest| rest.strip_suffix("/print"))
        .is_some_and(|id| !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}

/// Whether an answer to [`INFO_PATH`] says a Maker Connect hub is there.
pub fn read_info(status: u16, body: &str) -> bool {
    status == 200 && serde_json::from_str::<Value>(body).is_ok_and(|v| v.get("service").and_then(Value::as_str) == Some("maker-connect"))
}

pub fn info_request() -> HubRequest {
    HubRequest { method: "GET", path: INFO_PATH.to_string(), content_type: None, body: String::new() }
}

pub fn lasers_request() -> HubRequest {
    HubRequest { method: "GET", path: LASERS_PATH.to_string(), content_type: None, body: String::new() }
}

/// The lasers in the hub's answer, or why not.
pub fn read_lasers(status: u16, body: &str) -> Result<Vec<Laser>, String> {
    match status {
        200 => {
            let list: Value = serde_json::from_str(body).map_err(|_| no_answer())?;
            let list = list.as_array().ok_or_else(no_answer)?;
            Ok(list.iter().filter_map(laser).collect())
        }
        s => Err(refused(s, body)),
    }
}

fn laser(v: &Value) -> Option<Laser> {
    let area = v.get("work_area_mm").and_then(Value::as_array).and_then(|a| Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?]));
    Some(Laser {
        id: v.get("id")?.as_str()?.to_string(),
        name: v.get("name")?.as_str()?.to_string(),
        model: v.get("model").and_then(Value::as_str).map(str::to_string),
        online: v.get("online").and_then(Value::as_bool).unwrap_or(false),
        work_area_mm: area.filter(|[x, y]| x.is_finite() && y.is_finite()),
        laser_mode: v.pointer("/laser/laser_mode").and_then(Value::as_bool),
        max_s: v.pointer("/laser/max_s").and_then(Value::as_f64).filter(|s| s.is_finite() && *s >= 1.0 && *s <= 100_000.0).map(|s| s.round() as u32),
    })
}

/// The request that keeps `gcode` in the hub's Files as `name` (an `.gcode` file name). The text is exactly what LaserFoundry's own
/// exporter wrote.
pub fn upload_request(name: &str, gcode: &str) -> Result<HubRequest, String> {
    let name = gcode_name(name).ok_or("Give the drawing a name first.")?;
    if gcode.trim().is_empty() {
        return Err("Draw something first: there is nothing to send.".into());
    }
    if gcode.len() > MAX_GCODE_BYTES {
        return Err("The G-code is too big to send.".into());
    }
    Ok(HubRequest {
        method: "POST",
        path: format!("{FILES_PATH}?name={}", percent_encode(&name)),
        content_type: Some("text/plain"),
        body: gcode.to_string(),
    })
}

/// The id the hub gave the kept file, or why not.
pub fn read_upload(status: u16, body: &str) -> Result<String, String> {
    match status {
        201 => {
            let v: Value = serde_json::from_str(body).map_err(|_| no_answer())?;
            let id = v.get("id").and_then(Value::as_str).ok_or_else(no_answer)?;
            Ok(id.to_string())
        }
        s => Err(refused(s, body)),
    }
}

/// The request that loads a kept file on a laser; `replace` only after the person agreed to replace the loaded job.
pub fn load_request(file_id: &str, machine: &str, replace: bool) -> Result<HubRequest, String> {
    let clean = |s: &str| !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if !clean(file_id) || !clean(machine) {
        return Err(no_answer());
    }
    let body = if replace { format!("machine={machine}&replace=true") } else { format!("machine={machine}") };
    Ok(HubRequest { method: "POST", path: format!("/files/{file_id}/print"), content_type: Some("application/x-www-form-urlencoded"), body })
}

/// What the hub answered to [`load_request`]: loaded, asking whether to replace, or why not (in the hub's own words).
pub fn read_load(status: u16, body: &str) -> Result<Load, String> {
    let json = serde_json::from_str::<Value>(body).ok();
    match status {
        200 => {
            let file = json.as_ref().and_then(|v| v.get("loaded")).and_then(Value::as_str).ok_or_else(no_answer)?;
            Ok(Load::Loaded(file.to_string()))
        }
        409 if let Some(v) = json.as_ref().filter(|v| v.get("confirm").is_some()) => {
            let question = v.get("confirm").and_then(Value::as_str).ok_or_else(no_answer)?;
            let loaded = v.get("loaded").and_then(Value::as_str).unwrap_or_default();
            Ok(Load::Replace { loaded: loaded.to_string(), question: question.chars().take(300).collect() })
        }
        s => Err(refused(s, body)),
    }
}

/// The hub's refusal, its sentence as it is (a hub answers a refusal with plain text).
fn refused(status: u16, body: &str) -> String {
    let text: String = body.trim().chars().take(300).collect();
    match status {
        401 => "Sign in to the hub again, then try again.".to_string(),
        s if s >= 500 => format!("The hub failed (error {s}): try again later."),
        s if text.is_empty() => format!("The hub refused this (error {s})."),
        _ => text,
    }
}

fn no_answer() -> String {
    "The hub's answer was not what this page expects: is it a newer or older Maker Connect?".to_string()
}

/// `name` as a hub file name: letters, digits, spaces, hyphens and underscores (anything else becomes `_`), at most 60
/// characters, ending in `.gcode`.
pub fn gcode_name(name: &str) -> Option<String> {
    let stem = name.trim();
    let stem =
        stem.len().checked_sub(6).and_then(|at| stem.get(at..)?.eq_ignore_ascii_case(".gcode").then(|| stem.get(..at)).flatten()).unwrap_or(stem);
    let plain: String = stem.chars().map(|c| if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_') { c } else { '_' }).take(60).collect();
    let plain = plain.trim_matches(['_', '.', ' ']);
    (!plain.is_empty()).then(|| format!("{plain}.gcode"))
}

fn percent_encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
