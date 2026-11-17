# Maker Connect (fork)

A fork of the browser version for [Maker Connect](https://github.com/CuriousTimo/Maker-Connect), a makerspace hub that serves this page
from its own address. It follows the pattern of [Publish to MakerFoundry](makerfoundry.md): requests and answers in `crates/ai`
(`maker_connect.rs`, no network code), the window in `crates/ui-egui` (`maker_connect_ui.rs`), the browser's `fetch` in `apps/laserfoundry-web`.

## Send to a laser (File menu)
Shown only when the page's own origin answers `GET /api/apps/v1/info` as a Maker Connect. The window lists the lasers the signed-in person
may use (`GET /api/laser-design/lasers`), one **Send to <laser name>** button each. Sending:

1. keeps the drawing's G-code, exactly as **Save G-code** writes it, in the hub's Files (`POST /api/files?name=<name>.gcode`);
2. loads it as that laser's job (`POST /files/<id>/print`, form `machine`). A laser holds one loaded job: when one is there the hub answers
   `409 {"loaded", "confirm"}`, its question is shown, and the request is sent again with `replace=true` only after the person presses
   **Replace it on <laser>**.

Nothing starts: the hub only loads the job, and a person at the laser frames and starts it there. The page may send exactly these calls
(`maker_connect::path_allowed`), only to its own origin (`fetch` with `credentials: same-origin`, no redirects), with no token stored. When a
hub serves the page, the AI, account and MakerFoundry features are off (`http_available`).

## Without Web Serial
`cargo build -p laserfoundry-web --no-default-features` leaves out `serial_pick`, `serial_close`, `serial_signals` and `serial_web.rs` (the Web
Serial code, moved there unchanged). `machine_available()` is then false, and the app says "Connecting to a machine is not available in this
build". The default build is as before.

## Changed files
`NOTICE`, `ROADMAP.md`, `docs/maker-connect.md`, `apps/laserfoundry-web/Cargo.toml`, `apps/laserfoundry-web/src/lib.rs`,
`apps/laserfoundry-web/src/serial_web.rs` (new, moved code), `crates/ai/src/lib.rs`, `crates/ai/src/maker_connect.rs` (new),
`crates/ai/src/tests.rs`, `crates/fuzzkit/src/lib.rs`, `fuzz/Cargo.toml`, `fuzz/fuzz_targets/maker_connect.rs` (new),
`crates/ui-egui/src/lib.rs`, `crates/ui-egui/src/menus.rs`, `crates/ui-egui/src/platform.rs`, `crates/ui-egui/src/maker_connect_ui.rs` (new)
and `crates/ui-egui/src/maker_connect_ui/tests.rs` (new).
