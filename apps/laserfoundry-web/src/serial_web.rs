//! Web Serial: the machine link of the browser build, left out when the `machine-link` feature is off (modified by
//! Maker Connect: the code is moved here unchanged from `lib.rs` so the feature can leave it out).

use crate::serial_close::{CloseStep, Held, Teardown};
use crate::serial_pick::{PortId, choose};
use crate::serial_signals::{Moment, lines_at};
use laserfoundry_device::Transport;
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};

// ---- Web Serial ----------------------------------------------------------------------------
// The API is not in web-sys's stable set, so the few calls needed are declared here.

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["navigator", "serial"], js_name = requestPort, catch)]
    fn serial_request_port() -> Result<js_sys::Promise, JsValue>;

    #[wasm_bindgen(js_namespace = ["navigator", "serial"], js_name = getPorts, catch)]
    fn serial_get_ports() -> Result<js_sys::Promise, JsValue>;

    type SerialPort;
    #[wasm_bindgen(method, js_name = getInfo)]
    fn get_info(this: &SerialPort) -> JsValue;
    #[wasm_bindgen(method, js_name = open)]
    fn open(this: &SerialPort, options: &JsValue) -> js_sys::Promise;
    #[wasm_bindgen(method, js_name = close)]
    fn close(this: &SerialPort) -> js_sys::Promise;
    #[wasm_bindgen(method, js_name = setSignals)]
    fn set_signals(this: &SerialPort, signals: &JsValue) -> js_sys::Promise;
    #[wasm_bindgen(method, getter)]
    fn readable(this: &SerialPort) -> Stream;
    #[wasm_bindgen(method, getter)]
    fn writable(this: &SerialPort) -> Stream;

    type Stream;
    #[wasm_bindgen(method, js_name = getReader, catch)]
    fn get_reader(this: &Stream) -> Result<Reader, JsValue>;
    #[wasm_bindgen(method, js_name = getWriter, catch)]
    fn get_writer(this: &Stream) -> Result<Writer, JsValue>;

    type Reader;
    #[wasm_bindgen(method)]
    fn read(this: &Reader) -> js_sys::Promise;
    #[wasm_bindgen(method)]
    fn cancel(this: &Reader) -> js_sys::Promise;
    #[wasm_bindgen(method, js_name = releaseLock)]
    fn release_lock(this: &Reader);

    type Writer;
    #[wasm_bindgen(method)]
    fn write(this: &Writer, chunk: &JsValue) -> js_sys::Promise;
    #[wasm_bindgen(method)]
    fn close(this: &Writer) -> js_sys::Promise;
    #[wasm_bindgen(method, js_name = releaseLock)]
    fn release_lock(this: &Writer);
}

/// GRBL's usual speed.
const BAUD: f64 = 115_200.0;

pub(super) fn web_serial_available() -> bool {
    web_sys::window().is_some_and(|w| js_sys::Reflect::has(&w.navigator(), &JsValue::from_str("serial")).unwrap_or(false))
}

thread_local! {
    /// The device opened last, so a reconnect picks it again when several ports are granted.
    static LAST_PORT: std::cell::Cell<Option<PortId>> = const { std::cell::Cell::new(None) };
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // the value is range-checked just above the cast
fn port_id(port: &SerialPort) -> PortId {
    let info = port.get_info();
    let field = |name: &str| {
        js_sys::Reflect::get(&info, &JsValue::from_str(name))
            .ok()
            .and_then(|v| v.as_f64())
            .filter(|v| (0.0..=f64::from(u16::MAX)).contains(v))
            .map(|v| v as u16)
    };
    PortId { vendor: field("usbVendorId"), product: field("usbProductId") }
}

/// A port the person already granted, if one is the obvious choice (no picker on reconnect).
async fn granted_port() -> Option<SerialPort> {
    let list = JsFuture::from(serial_get_ports().ok()?).await.ok()?;
    let ports: Vec<SerialPort> = js_sys::Array::from(&list).iter().map(|p| p.unchecked_into()).collect();
    let ids: Vec<PortId> = ports.iter().map(port_id).collect();
    let index = choose(&ids, LAST_PORT.with(|l| l.get()))?;
    ports.into_iter().nth(index)
}

/// Reopens an already-granted port, or asks the person to choose a serial device; opens it and starts the reader.
pub(super) async fn open_serial_port() -> Result<Box<dyn Transport + Send>, JsValue> {
    if !web_serial_available() {
        return Err(JsValue::from_str("this browser has no Web Serial (use a Chromium-based browser)"));
    }
    let port: SerialPort = match granted_port().await {
        Some(port) => port,
        None => JsFuture::from(serial_request_port()?).await?.unchecked_into(),
    };
    LAST_PORT.with(|l| l.set(Some(port_id(&port))));
    let options = js_sys::Object::new();
    js_sys::Reflect::set(&options, &JsValue::from_str("baudRate"), &JsValue::from_f64(BAUD))?;
    JsFuture::from(port.open(&options)).await?;
    lower_signals(&port, Moment::AfterOpen).await;
    // From here the port is open: any failure below must close it again or the next Connect cannot open it.
    let port = Rc::new(port);
    let reader = match port.readable().get_reader() {
        Ok(reader) => Rc::new(reader),
        Err(e) => {
            close_port_steps(&port, None, None, Teardown::default().steps(Held::default())).await;
            return Err(e);
        }
    };
    let writer = match port.writable().get_writer() {
        Ok(writer) => Rc::new(writer),
        Err(e) => {
            let steps = Teardown::default().steps(Held { reader: true, writer: false });
            close_port_steps(&port, Some(&*reader), None, steps).await;
            return Err(e);
        }
    };
    let inbox = Rc::new(RefCell::new(Inbox::default()));
    let task_reader = Rc::clone(&reader);
    let reading = Rc::clone(&inbox);
    spawn_local(async move {
        loop {
            let Ok(chunk) = JsFuture::from(task_reader.read()).await else { break };
            let done = js_sys::Reflect::get(&chunk, &JsValue::from_str("done")).ok().and_then(|d| d.as_bool()).unwrap_or(true);
            if done {
                break;
            }
            if let Ok(value) = js_sys::Reflect::get(&chunk, &JsValue::from_str("value")) {
                let mut inbox = reading.borrow_mut();
                inbox.bytes.extend(js_sys::Uint8Array::new(&value).to_vec());
                // A device at the wrong speed (or a hostile one) may never send a line end: keep only the newest bytes.
                let excess = inbox.bytes.len().saturating_sub(MAX_INBOX_BYTES);
                inbox.bytes.drain(..excess);
            }
        }
        reading.borrow_mut().closed = true;
    });
    let outbox = Rc::new(RefCell::new(Outbox::default()));
    Ok(Box::new(WebSerial { inbox, outbox, port, reader, writer, teardown: Teardown::default() }))
}

/// Shuts the port down in the order [`Teardown`] gives. Every step is attempted even if one fails.
async fn close_port_steps(port: &SerialPort, reader: Option<&Reader>, writer: Option<&Writer>, steps: Vec<CloseStep>) {
    for step in steps {
        match (step, reader, writer) {
            (CloseStep::CancelReader, Some(r), _) => drop(JsFuture::from(r.cancel()).await),
            (CloseStep::CloseWriter, _, Some(w)) => drop(JsFuture::from(w.close()).await),
            (CloseStep::ReleaseReader, Some(r), _) => r.release_lock(),
            (CloseStep::ReleaseWriter, _, Some(w)) => w.release_lock(),
            (CloseStep::ClosePort, ..) => {
                lower_signals(port, Moment::BeforeClose).await;
                drop(JsFuture::from(port.close()).await);
            }
            _ => {}
        }
    }
}

/// Drives DTR and RTS to the state [`lines_at`] gives, so an auto-reset board is not held in reset or sent into its bootloader.
/// Best effort: a device without these lines refuses, which is fine. Nothing is written to the controller.
async fn lower_signals(port: &SerialPort, moment: Moment) {
    let lines = lines_at(moment);
    let signals = js_sys::Object::new();
    for (name, on) in [("dataTerminalReady", lines.data_terminal_ready), ("requestToSend", lines.request_to_send)] {
        if js_sys::Reflect::set(&signals, &JsValue::from_str(name), &JsValue::from_bool(on)).is_err() {
            return;
        }
    }
    drop(JsFuture::from(port.set_signals(&signals)).await);
}

/// Most unread serial bytes kept (GRBL's replies are a few dozen bytes a line).
const MAX_INBOX_BYTES: usize = 64 * 1024;

#[derive(Default)]
struct Inbox {
    bytes: std::collections::VecDeque<u8>,
    closed: bool,
}

#[derive(Default)]
struct Outbox {
    chunks: std::collections::VecDeque<Vec<u8>>,
    pumping: bool,
}

/// A serial device as a non-blocking [`Transport`]: reads come from what the reader task has
/// collected, and writes are queued for a task that sends them in order.
struct WebSerial {
    inbox: Rc<RefCell<Inbox>>,
    outbox: Rc<RefCell<Outbox>>,
    port: Rc<SerialPort>,
    reader: Rc<Reader>,
    writer: Rc<Writer>,
    teardown: Teardown,
}

/// Disconnecting, a lost connection and a failed handshake all end by dropping the transport: close the
/// port then, or the tab keeps it open and locked and the next Connect fails.
impl Drop for WebSerial {
    fn drop(&mut self) {
        let steps = self.teardown.steps(Held { reader: true, writer: true });
        let (port, reader, writer) = (Rc::clone(&self.port), Rc::clone(&self.reader), Rc::clone(&self.writer));
        self.inbox.borrow_mut().closed = true;
        spawn_local(async move { close_port_steps(&port, Some(&*reader), Some(&*writer), steps).await });
    }
}

// SAFETY-ish note: the browser build is single-threaded (wasm32 without atomics has no other
// thread to move this to), so the `Send` the link's API asks for cannot be violated.
unsafe impl Send for WebSerial {}

impl Transport for WebSerial {
    fn write_all(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        if self.inbox.borrow().closed {
            return Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "the serial device was closed"));
        }
        let start = {
            let mut out = self.outbox.borrow_mut();
            out.chunks.push_back(bytes.to_vec());
            !std::mem::replace(&mut out.pumping, true)
        };
        if start {
            let (outbox, writer, inbox) = (Rc::clone(&self.outbox), Rc::clone(&self.writer), Rc::clone(&self.inbox));
            spawn_local(async move {
                loop {
                    let next = {
                        let mut out = outbox.borrow_mut();
                        let next = out.chunks.pop_front();
                        if next.is_none() {
                            out.pumping = false;
                        }
                        next
                    };
                    let Some(chunk) = next else { break };
                    if JsFuture::from(writer.write(&js_sys::Uint8Array::from(chunk.as_slice()))).await.is_err() {
                        inbox.borrow_mut().closed = true;
                        outbox.borrow_mut().pumping = false;
                        break;
                    }
                }
            });
        }
        Ok(())
    }

    fn read_line(&mut self, _timeout: std::time::Duration) -> std::io::Result<Option<String>> {
        let mut inbox = self.inbox.borrow_mut();
        if let Some(end) = inbox.bytes.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = inbox.bytes.drain(..=end).collect();
            return Ok(Some(String::from_utf8_lossy(&line).trim_end_matches(['\r', '\n']).to_string()));
        }
        if inbox.closed {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "the serial device was closed"));
        }
        Ok(None)
    }
}
