//! LaserFoundry in the browser: the same engine and egui interface, compiled to WebAssembly.
//!
//! Files open through the browser's file chooser and save as downloads. There is no machine link
//! yet (the browser's Web Serial API is the way to add one). Build it with `apps/laserfoundry-web/build.sh`.
//!
//! The wasm-bindgen glue is generated code, so the workspace's `unsafe_code` lint is relaxed here.
#![allow(unsafe_code)]

pub mod fontindex;
mod page;
// Web Serial's pure parts: left out with the machine link (the `machine-link` feature; Maker Connect's change).
#[cfg(feature = "machine-link")]
pub mod serial_close;
#[cfg(feature = "machine-link")]
pub mod serial_pick;
#[cfg(feature = "machine-link")]
pub mod serial_signals;
pub mod service;

#[cfg(all(target_arch = "wasm32", feature = "machine-link"))]
mod serial_web;

#[cfg(target_arch = "wasm32")]
mod web {
    use eframe::wasm_bindgen::JsCast;
    use laserfoundry_ai::services::Services;
    use laserfoundry_engine::Session;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    #[cfg(feature = "machine-link")]
    use crate::serial_web::{open_serial_port, web_serial_available};

    use laserfoundry_device::Transport;
    use laserfoundry_ui_egui::{Fetched, FontDownload, FontDownloadFailure, LaserFoundryApp, Platform, Upload};
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::{JsFuture, spawn_local};

    /// Starts the app on the `<canvas>` with the given id.
    #[wasm_bindgen]
    pub async fn start(canvas_id: String) -> Result<(), JsValue> {
        let document = web_sys::window().and_then(|w| w.document()).ok_or_else(|| JsValue::from_str("no document"))?;
        let canvas = document
            .get_element_by_id(&canvas_id)
            .ok_or_else(|| JsValue::from_str("no canvas with that id"))?
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .map_err(|_| JsValue::from_str("that element is not a canvas"))?;
        restore_fonts();
        // Whether a Maker Connect hub served this page (Maker Connect's change): asked of the page's own origin only.
        let hub_found = hub_serves_this_page().await;
        let leave_warning = Rc::new(Cell::new(false));
        install_leave_warning(&leave_warning);
        eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(move |_cc| {
                    Ok(Box::new(Host {
                        app: LaserFoundryApp::new(
                            Session::new(),
                            Box::new(WebPlatform {
                                inbox: Rc::new(RefCell::new(None)),
                                serial: Rc::new(RefCell::new(None)),
                                hub: hub_found,
                                hub_job: Rc::new(RefCell::new(HubJob::default())),
                                font_job: Rc::new(RefCell::new(FontJob::default())),
                                leave_warning,
                                http_job: Rc::new(RefCell::new(HttpJob::default())),
                                link: RefCell::new(take_open_link()),
                                services: read_services(),
                                fetch_job: Rc::new(RefCell::new(FetchJob::default())),
                            }),
                        ),
                    }))
                }),
            )
            .await
    }

    /// Asks the browser to confirm before the page is closed or reloaded while `flag` is set. A page cannot send
    /// anything to the machine while it is being torn down, so a job stopped this way would run on unsupervised.
    fn install_leave_warning(flag: &Rc<Cell<bool>>) {
        let flag = Rc::clone(flag);
        let handler = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
            if flag.get() {
                event.prevent_default();
            }
        });
        if let Some(window) = web_sys::window()
            && window.add_event_listener_with_callback("beforeunload", handler.as_ref().unchecked_ref()).is_err()
        {
            log::warn!("could not watch for the page closing");
        }
        handler.forget(); // lives as long as the page
    }

    /// Files go through the browser's file chooser and downloads; there are no paths, serial ports
    /// or threads for a machine link.
    struct WebPlatform {
        /// The chosen file once it has been read (shared with the browser callbacks).
        inbox: Rc<RefCell<Option<Upload>>>,
        /// The serial device once the person has chosen it and it is open (or why not).
        #[cfg_attr(not(feature = "machine-link"), allow(dead_code))]
        serial: Rc<RefCell<Option<Result<Box<dyn Transport + Send>, String>>>>,
        /// Whether a Maker Connect hub served this page (the File menu then offers "Send to a laser…").
        hub: bool,
        /// The request to that hub in flight (or its answer, not yet taken).
        hub_job: Rc<RefCell<HubJob>>,
        font_job: Rc<RefCell<FontJob>>,
        /// Whether closing the page would lose work or stop a job halfway (read by the `beforeunload` handler).
        leave_warning: Rc<Cell<bool>>,
        /// The AI or account request in flight (or its answer, not yet taken).
        http_job: Rc<RefCell<HttpJob>>,
        /// The design address the page was opened with (`?open=`), until the app has taken it.
        link: RefCell<Option<String>>,
        /// The real services, or the staging pair this tab was asked to use (`?services=staging`).
        services: Services,
        /// The design being fetched from a link (or its answer, not yet taken).
        fetch_job: Rc<RefCell<FetchJob>>,
    }

    impl Platform for WebPlatform {
        fn pick_open(&self, _: &str, _: &[&str]) -> Option<std::path::PathBuf> {
            None
        }

        fn pick_save(&self, _: &str, _: &str, _: &[&str]) -> Option<std::path::PathBuf> {
            None
        }

        fn browser_files(&self) -> bool {
            true
        }

        fn request_upload(&self, extensions: &[&str]) {
            if let Err(e) = choose_file(extensions, Rc::clone(&self.inbox)) {
                log::warn!("could not open the file chooser: {e:?}");
            }
        }

        fn take_upload(&self) -> Option<Upload> {
            self.inbox.borrow_mut().take()
        }

        fn download(&self, name: &str, bytes: &[u8], mime: &str) {
            if let Err(e) = start_download(name, bytes, mime) {
                log::warn!("could not start the download: {e:?}");
            }
        }

        fn storage_get(&self, key: &str) -> Option<String> {
            // Storage can be missing or throw (private windows, blocked site data): then nothing is remembered.
            web_sys::window()?.local_storage().ok()??.get_item(key).ok()?
        }

        fn http_available(&self) -> bool {
            // served by a Maker Connect hub (Maker Connect's change): the page talks to that hub only, not to the AI services
            !self.hub && BUILT_IN_SERVICE.is_some_and(|s| crate::service::origin(s).is_some())
        }

        fn start_http(&self, request: laserfoundry_ai::HttpRequest) -> bool {
            {
                let mut job = self.http_job.borrow_mut();
                if job.running {
                    return false;
                }
                job.running = true;
                job.result = None;
            }
            let job = Rc::clone(&self.http_job);
            let services = self.services;
            spawn_local(async move {
                let result = if crate::service::post_allowed(&request.url, BUILT_IN_SERVICE, services) {
                    post(&request).await
                } else {
                    Err("this page can only talk to the service it was built for and to MakerFoundry".to_string())
                };
                let mut job = job.borrow_mut();
                job.running = false;
                job.result = Some(result);
            });
            true
        }

        fn take_http(&self) -> Option<Result<(u16, String), String>> {
            self.http_job.borrow_mut().result.take()
        }

        fn services(&self) -> Services {
            self.services
        }

        fn startup_link(&self) -> Option<String> {
            self.link.borrow_mut().take()
        }

        fn start_fetch(&self, url: &str, max_bytes: usize) -> bool {
            {
                let mut job = self.fetch_job.borrow_mut();
                if job.running {
                    return false;
                }
                job.running = true;
                job.result = None;
            }
            let job = Rc::clone(&self.fetch_job);
            let url = url.to_string();
            let services = self.services;
            spawn_local(async move {
                let result = if crate::service::fetch_allowed(&url, services) {
                    get(&url, max_bytes).await
                } else {
                    Err("this page only fetches designs from MakerFoundry".to_string())
                };
                let mut job = job.borrow_mut();
                job.running = false;
                job.result = Some(result);
            });
            true
        }

        fn take_fetch(&self) -> Option<Result<Fetched, String>> {
            self.fetch_job.borrow_mut().result.take()
        }

        fn set_leave_warning(&self, on: bool) {
            self.leave_warning.set(on);
        }

        fn storage_set(&self, key: &str, value: &str) {
            let stored = web_sys::window().and_then(|w| w.local_storage().ok()).flatten().map(|s| s.set_item(key, value));
            if !matches!(stored, Some(Ok(()))) {
                log::warn!("could not remember the recent projects (browser storage is unavailable or full)");
            }
        }

        fn font_library_available(&self) -> bool {
            true
        }

        fn font_library_everything_available(&self) -> bool {
            false
        }

        fn library_fonts(&self) -> Vec<(String, String)> {
            laserfoundry_engine::fontlib::list().into_iter().map(|(name, display)| (display, name)).collect()
        }

        fn start_font_library_download(&self, _everything: bool) -> bool {
            {
                let mut job = self.font_job.borrow_mut();
                if job.running {
                    return false;
                }
                job.running = true;
                job.result = None;
            }
            let job = Rc::clone(&self.font_job);
            spawn_local(async move {
                let result = load_fonts(None).await.map(|folders| {
                    if let (Some(storage), Ok(text)) = (local_storage(), serde_json::to_string(&folders)) {
                        let _ = storage.set_item(FONTS_KEY, &text);
                    }
                    FontDownload::Loaded { fonts: folders.len() }
                });
                let mut job = job.borrow_mut();
                job.running = false;
                job.result = Some(result);
            });
            true
        }

        fn take_font_library_result(&self) -> Option<Result<FontDownload, FontDownloadFailure>> {
            self.font_job.borrow_mut().result.take()
        }

        fn inline_link(&self) -> bool {
            true
        }

        fn hub_available(&self) -> bool {
            self.hub
        }

        fn start_hub(&self, request: laserfoundry_ai::maker_connect::HubRequest) -> bool {
            if !self.hub || !laserfoundry_ai::maker_connect::path_allowed(&request.path) {
                return false;
            }
            {
                let mut job = self.hub_job.borrow_mut();
                if job.running {
                    return false;
                }
                job.running = true;
                job.result = None;
            }
            let job = Rc::clone(&self.hub_job);
            spawn_local(async move {
                let result = hub_call(&request).await;
                let mut job = job.borrow_mut();
                job.running = false;
                job.result = Some(result);
            });
            true
        }

        fn take_hub(&self) -> Option<Result<(u16, String), String>> {
            self.hub_job.borrow_mut().result.take()
        }

        // Without the `machine-link` feature the page has no machine link at all: no serial, and LaserFoundry's own
        // "not available in this build" text where a machine would be connected.
        #[cfg(not(feature = "machine-link"))]
        fn machine_available(&self) -> bool {
            false
        }

        #[cfg(feature = "machine-link")]
        fn serial_async(&self) -> bool {
            true
        }

        #[cfg(feature = "machine-link")]
        fn request_serial(&self) {
            let slot = Rc::clone(&self.serial);
            spawn_local(async move {
                let result = open_serial_port().await.map_err(describe);
                *slot.borrow_mut() = Some(result);
            });
        }

        #[cfg(feature = "machine-link")]
        fn take_serial(&self) -> Option<Result<Box<dyn Transport + Send>, String>> {
            self.serial.borrow_mut().take()
        }

        fn serial_ports(&self) -> Vec<String> {
            #[cfg(feature = "machine-link")]
            if web_serial_available() {
                return vec!["USB serial device".into()];
            }
            Vec::new()
        }

        fn open_serial(&self, _: &str) -> Result<Box<dyn Transport + Send>, String> {
            Err("choose the device in the browser's prompt".into())
        }
    }

    /// Opens the browser's file chooser; the chosen file lands in `inbox` once read.
    fn choose_file(extensions: &[&str], inbox: Rc<RefCell<Option<Upload>>>) -> Result<(), JsValue> {
        let document = web_sys::window().and_then(|w| w.document()).ok_or_else(|| JsValue::from_str("no document"))?;
        let input = document.create_element("input")?.dyn_into::<web_sys::HtmlInputElement>()?;
        input.set_type("file");
        input.set_accept(&extensions.iter().map(|e| format!(".{e}")).collect::<Vec<_>>().join(","));
        let chosen = input.clone();
        let on_change = Closure::once(move || {
            let Some(file) = chosen.files().and_then(|f| f.get(0)) else { return };
            let name = file.name();
            let Ok(reader) = web_sys::FileReader::new() else { return };
            let done = reader.clone();
            let on_load = Closure::once(move || {
                if let Ok(result) = done.result() {
                    let bytes = js_sys::Uint8Array::new(&result).to_vec();
                    *inbox.borrow_mut() = Some(Upload { name, bytes });
                }
            });
            reader.set_onload(Some(on_load.as_ref().unchecked_ref()));
            on_load.forget();
            if let Err(e) = reader.read_as_array_buffer(&file) {
                log::warn!("could not read the file: {e:?}");
            }
        });
        input.add_event_listener_with_callback("change", on_change.as_ref().unchecked_ref())?;
        on_change.forget();
        input.click();
        Ok(())
    }

    /// Saves `bytes` through a temporary link, which is how a page triggers a download.
    fn start_download(name: &str, bytes: &[u8], mime: &str) -> Result<(), JsValue> {
        let document = web_sys::window().and_then(|w| w.document()).ok_or_else(|| JsValue::from_str("no document"))?;
        let array = js_sys::Uint8Array::from(bytes);
        let parts = js_sys::Array::of1(&array);
        let options = web_sys::BlobPropertyBag::new();
        options.set_type(mime);
        let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options)?;
        let url = web_sys::Url::create_object_url_with_blob(&blob)?;
        let link = document.create_element("a")?.dyn_into::<web_sys::HtmlAnchorElement>()?;
        link.set_href(&url);
        link.set_download(name);
        link.click();
        web_sys::Url::revoke_object_url(&url)
    }

    pub(crate) fn describe(e: JsValue) -> String {
        e.as_string()
            .or_else(|| js_sys::Reflect::get(&e, &JsValue::from_str("message")).ok().and_then(|m| m.as_string()))
            .unwrap_or_else(|| "the browser refused".into())
    }

    /// Where the installed font folders are remembered between visits.
    const FONTS_KEY: &str = "laserfoundry.fonts";

    /// The font library download in progress (or its result, not yet taken).
    #[derive(Default)]
    struct FontJob {
        running: bool,
        result: Option<Result<FontDownload, FontDownloadFailure>>,
    }

    /// The AI or account request in flight (or its answer, not yet taken).
    #[derive(Default)]
    struct HttpJob {
        running: bool,
        result: Option<Result<(u16, String), String>>,
    }

    /// The request to the hub that served the page (or its answer, not yet taken).
    #[derive(Default)]
    struct HubJob {
        running: bool,
        result: Option<Result<(u16, String), String>>,
    }

    /// The design being fetched from a link (or its answer, not yet taken).
    #[derive(Default)]
    struct FetchJob {
        running: bool,
        result: Option<Result<Fetched, String>>,
    }

    /// The tab's services: `?services=` and what this tab remembered (session storage, so only this tab, only for now).
    fn read_services() -> Services {
        const KEY: &str = "laserfoundry.services";
        let window = web_sys::window();
        let query = window
            .as_ref()
            .and_then(|w| w.location().search().ok())
            .and_then(|q| web_sys::UrlSearchParams::new_with_str(&q).ok())
            .and_then(|p| p.get("services"));
        let storage = window.as_ref().and_then(|w| w.session_storage().ok()).flatten();
        let remembered = storage.as_ref().and_then(|s| s.get_item(KEY).ok()).flatten();
        let (services, remember) = crate::service::choose_services(query.as_deref(), remembered.as_deref());
        if let (Some(storage), Some(on)) = (storage, remember) {
            // storage can throw (blocked site data): the choice then lasts until the next reload only
            let _ = if on { storage.set_item(KEY, "staging") } else { storage.remove_item(KEY) };
        }
        services
    }

    /// Reads the design address the page was opened with (`?open=`) and takes it out of the page's address, so reloading the
    /// page does not fetch it again or put the design back after it was changed.
    fn take_open_link() -> Option<String> {
        let window = web_sys::window()?;
        let location = window.location();
        let params = web_sys::UrlSearchParams::new_with_str(&location.search().ok()?).ok()?;
        let value = params.get("open")?;
        params.delete("open");
        let rest = params.to_string().as_string().unwrap_or_default();
        let path = location.pathname().ok()?;
        let hash = location.hash().unwrap_or_default();
        let address = format!("{path}{}{hash}", if rest.is_empty() { String::new() } else { format!("?{rest}") });
        if let Ok(history) = window.history() {
            let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(&address));
        }
        Some(value)
    }

    /// Fetches `url` with a GET: no cookies or other credentials, no redirects, a time limit, and at most `max_bytes` of
    /// answer. The address was checked by the caller and by [`crate::service::fetch_allowed`].
    async fn get(url: &str, max_bytes: usize) -> Result<Fetched, String> {
        let window = web_sys::window().ok_or("no window")?;
        let controller = web_sys::AbortController::new().map_err(describe)?;
        let init = web_sys::RequestInit::new();
        init.set_method("GET");
        init.set_credentials(web_sys::RequestCredentials::Omit);
        init.set_redirect(web_sys::RequestRedirect::Error);
        init.set_signal(Some(&controller.signal()));
        let abort = Closure::once_into_js(move || controller.abort());
        let timer = window.set_timeout_with_callback_and_timeout_and_arguments_0(abort.unchecked_ref(), HTTP_TIMEOUT_MS).ok();
        let answer = async {
            let response: web_sys::Response = JsFuture::from(window.fetch_with_str_and_init(url, &init))
                .await
                .map_err(describe)?
                .dyn_into()
                .map_err(|_| "the browser gave no response".to_string())?;
            let too_big = || format!("the design is larger than the {} MB this page reads from a link", max_bytes / 1024 / 1024);
            let declared = response.headers().get("content-length").ok().flatten().and_then(|n| n.parse::<usize>().ok());
            if declared.is_some_and(|n| n > max_bytes) {
                return Err(too_big());
            }
            let content_disposition = response.headers().get("content-disposition").ok().flatten();
            let status = response.status();
            let buffer = JsFuture::from(response.array_buffer().map_err(describe)?).await.map_err(describe)?;
            let bytes = js_sys::Uint8Array::new(&buffer);
            if bytes.length() as usize > max_bytes {
                return Err(too_big());
            }
            Ok(Fetched { status, bytes: bytes.to_vec(), content_disposition })
        }
        .await;
        if let Some(timer) = timer {
            window.clear_timeout_with_handle(timer);
        }
        answer
    }

    /// Sends `request` to the page's own origin with the person's session cookie (`same-origin`: never to another site), and
    /// returns the status and body. The path was checked by [`laserfoundry_ai::maker_connect::path_allowed`].
    async fn hub_call(request: &laserfoundry_ai::maker_connect::HubRequest) -> Result<(u16, String), String> {
        let window = web_sys::window().ok_or("no window")?;
        let controller = web_sys::AbortController::new().map_err(describe)?;
        let init = web_sys::RequestInit::new();
        init.set_method(request.method);
        init.set_credentials(web_sys::RequestCredentials::SameOrigin);
        init.set_redirect(web_sys::RequestRedirect::Error);
        init.set_signal(Some(&controller.signal()));
        let headers = web_sys::Headers::new().map_err(describe)?;
        // the hub answers a form sent by a page as data, not as a redirect to another page
        headers.set("x-requested-with", "laserfoundry").map_err(describe)?;
        if let Some(content_type) = request.content_type {
            headers.set("content-type", content_type).map_err(describe)?;
        }
        init.set_headers(&headers);
        if request.method == "POST" {
            init.set_body(&JsValue::from_str(&request.body));
        }
        let abort = Closure::once_into_js(move || controller.abort());
        let timer = window.set_timeout_with_callback_and_timeout_and_arguments_0(abort.unchecked_ref(), HTTP_TIMEOUT_MS).ok();
        let answer = async {
            let response: web_sys::Response = JsFuture::from(window.fetch_with_str_and_init(&request.path, &init))
                .await
                .map_err(describe)?
                .dyn_into()
                .map_err(|_| "the browser gave no response".to_string())?;
            let status = response.status();
            let text = JsFuture::from(response.text().map_err(describe)?).await.map_err(describe)?.as_string().ok_or("the answer is not text")?;
            if text.len() > MAX_ANSWER {
                return Err("the answer was larger than expected".to_string());
            }
            Ok((status, text))
        }
        .await;
        if let Some(timer) = timer {
            window.clear_timeout_with_handle(timer);
        }
        answer
    }

    /// Whether the hub's `info` call answers on this page's own origin, as a Maker Connect (a page from anywhere else gets
    /// no such answer and the menu entry stays away).
    async fn hub_serves_this_page() -> bool {
        match hub_call(&laserfoundry_ai::maker_connect::info_request()).await {
            Ok((status, body)) => laserfoundry_ai::maker_connect::read_info(status, &body),
            Err(_) => false,
        }
    }

    /// The service this page was built for (`LF_CLOUD_URL` when the site was built, else the shared Foundry service).
    const BUILT_IN_SERVICE: Option<&str> = Some(laserfoundry_ai::cloud::built_in_server());
    /// How long a request may take, and the most of an answer read.
    const HTTP_TIMEOUT_MS: i32 = 240_000;
    const MAX_ANSWER: usize = 16 * 1024 * 1024;

    /// Sends `request` with `fetch` (no redirects, a time limit) and returns the status and body.
    async fn post(request: &laserfoundry_ai::HttpRequest) -> Result<(u16, String), String> {
        let window = web_sys::window().ok_or("no window")?;
        let headers = web_sys::Headers::new().map_err(describe)?;
        for (name, value) in &request.headers {
            headers.set(name, value).map_err(describe)?;
        }
        let controller = web_sys::AbortController::new().map_err(describe)?;
        let init = web_sys::RequestInit::new();
        init.set_method("POST");
        init.set_headers(&headers);
        init.set_body(&JsValue::from_str(&request.body));
        init.set_redirect(web_sys::RequestRedirect::Error);
        init.set_signal(Some(&controller.signal()));
        let abort = Closure::once_into_js(move || controller.abort());
        let timer = window.set_timeout_with_callback_and_timeout_and_arguments_0(abort.unchecked_ref(), HTTP_TIMEOUT_MS).ok();
        let answer = async {
            let response: web_sys::Response = JsFuture::from(window.fetch_with_str_and_init(&request.url, &init))
                .await
                .map_err(describe)?
                .dyn_into()
                .map_err(|_| "the browser gave no response".to_string())?;
            let status = response.status();
            let text = JsFuture::from(response.text().map_err(describe)?).await.map_err(describe)?.as_string().ok_or("the answer is not text")?;
            if text.len() > MAX_ANSWER {
                return Err("the answer was larger than expected".to_string());
            }
            Ok((status, text))
        }
        .await;
        if let Some(timer) = timer {
            window.clear_timeout_with_handle(timer);
        }
        answer
    }

    fn local_storage() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok()?
    }

    /// Fetches a text file from this site (same origin: the fonts are copied onto the site when it is
    /// built, so nothing here talks to a third party). `cap` bounds what is accepted.
    async fn fetch_text(url: &str, cap: usize) -> Result<String, String> {
        let window = web_sys::window().ok_or("no window")?;
        let response: web_sys::Response =
            JsFuture::from(window.fetch_with_str(url)).await.map_err(describe)?.dyn_into().map_err(|_| "the browser gave no response".to_string())?;
        if !response.ok() {
            return Err(format!("{} for {url}", response.status()));
        }
        let text = JsFuture::from(response.text().map_err(describe)?).await.map_err(describe)?.as_string().ok_or("the file is not text")?;
        if text.len() > cap {
            return Err(format!("{url} is larger than expected"));
        }
        Ok(text)
    }

    /// Downloads the fonts the site carries (only `only`, when given) into the engine's font registry.
    /// Returns the folders that loaded.
    async fn load_fonts(only: Option<Vec<String>>) -> Result<Vec<String>, FontDownloadFailure> {
        let index = fetch_text("fonts/index.json", crate::fontindex::MAX_INDEX_BYTES).await.map_err(FontDownloadFailure::SiteUnavailable)?;
        let mut loaded = Vec::new();
        for entry in crate::fontindex::parse_index(&index).map_err(FontDownloadFailure::Other)? {
            if only.as_ref().is_some_and(|names| !names.contains(&entry.folder)) {
                continue;
            }
            let base = format!("fonts/{}/", entry.folder);
            let info = fetch_text(&format!("{base}font.json"), entry.font_json_bytes as usize + 1024).await.map_err(FontDownloadFailure::Other)?;
            let glyphs =
                fetch_text(&format!("{base}glyphs.stitchfoundry"), entry.glyphs_bytes as usize + 1024).await.map_err(FontDownloadFailure::Other)?;
            match laserfoundry_engine::fontlib::register(&entry.folder, &info, &glyphs) {
                Ok(()) => loaded.push(entry.folder),
                Err(e) => log::warn!("skipping the font {}: {e}", entry.folder),
            }
        }
        if loaded.is_empty() && only.is_none() {
            return Err(FontDownloadFailure::NoUsableFonts);
        }
        Ok(loaded)
    }

    /// Re-fetches the fonts installed on an earlier visit (the browser's cache makes this cheap).
    fn restore_fonts() {
        let remembered =
            local_storage().and_then(|s| s.get_item(FONTS_KEY).ok().flatten()).map(|t| crate::fontindex::parse_remembered(&t)).unwrap_or_default();
        if remembered.is_empty() {
            return;
        }
        spawn_local(async move {
            if let Err(e) = load_fonts(Some(remembered)).await {
                log::warn!("could not restore the installed fonts: {}", e.text());
            }
        });
    }

    struct Host {
        app: LaserFoundryApp,
    }

    impl eframe::App for Host {
        fn ui(&mut self, ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {
            self.app.ui(ui);
        }
    }
}
