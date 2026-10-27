//! Browser front end for the AVEO Cheetah3 USB microscope.
//!
//! Talks to the device directly over WebUSB (Chromium browsers only). On
//! Windows the device must first be bound to the WinUSB driver (see README),
//! and because Chromium there has no isochronous transfers, the firmware is
//! patched to stream over a bulk endpoint instead (`scope_core::patch`).

mod diag;
mod usb;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use scope_core::firmware::CONFIRMED_AGAINST_VENDOR_MEDIA;
use scope_core::frame::BULK_HEADER_LEN;
use scope_core::protocol::{self, Resolution};
use scope_core::{yuv422_to_rgba, ByteOrder, Control, Firmware, FrameAssembler};
use wasm_bindgen::prelude::*;
use wasm_bindgen::{Clamped, JsCast};
use wasm_bindgen_futures::spawn_local;
use web_sys::{
    CanvasRenderingContext2d, Document, HtmlAnchorElement, HtmlButtonElement, HtmlCanvasElement,
    HtmlElement, HtmlInputElement, HtmlSelectElement, ImageData, UsbDevice,
};

const FW_STORAGE_KEY: &str = "lab-microscope.firmware.v1";
/// Isochronous transfers kept queued at once, so the bus never idles.
const TRANSFERS_IN_FLIGHT: usize = 6;
/// Packets (microframes, 125 µs each) per transfer.
const PACKETS_PER_TRANSFER: usize = 32;
/// Bulk transfers kept queued at once. With fewer than several in flight the
/// bridge's FIFO overflows between transfers and frames come up short.
const BULK_TRANSFERS_IN_FLIGHT: usize = 8;
const BULK_TRANSFER_LEN: u32 = 64 * 1024;
/// How long to look for the microscope after asking it to reconnect. It is
/// back within about a second; Chrome usually forgets it anyway.
const RECONNECT_WAIT_MS: u32 = 3000;

/// How video leaves the device: the vendor firmware's isochronous endpoint,
/// or the patched firmware's bulk endpoint (needed on Windows).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Transport {
    Iso,
    Bulk,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Disconnected,
    Busy,
    Ready,
    Streaming,
}

struct Ui {
    doc: Document,
    canvas: HtmlCanvasElement,
    ctx: CanvasRenderingContext2d,
    log: HtmlElement,
    log_panel: HtmlElement,
    status: HtmlElement,
    fw_status: HtmlElement,
    stats: HtmlElement,
    connect: HtmlButtonElement,
    start: HtmlButtonElement,
    stop: HtmlButtonElement,
    snapshot: HtmlButtonElement,
    diag: HtmlButtonElement,
    resolution: HtmlSelectElement,
    order: HtmlSelectElement,
    controls: HtmlElement,
}

struct State {
    ui: Ui,
    phase: Phase,
    firmware: Option<Firmware>,
    /// Device that has received firmware since it was plugged in.
    fw_uploaded_to: Option<UsbDevice>,
    device: Option<UsbDevice>,
    transport: Transport,
    /// Set while the device is asked to drop off the bus and come back, so
    /// that its disconnect is not reported as an unplug.
    reconnecting: bool,
    /// Incremented each time streaming starts, so stale loops can tell they are stale.
    generation: u32,
    assembler: FrameAssembler,
    rgba: Vec<u8>,
    resolution: Resolution,
    order: ByteOrder,
    frame_times: VecDeque<f64>,
}

type App = Rc<RefCell<State>>;

fn el<T: JsCast>(doc: &Document, id: &str) -> T {
    doc.get_element_by_id(id)
        .unwrap_or_else(|| panic!("missing #{id}"))
        .dyn_into::<T>()
        .unwrap_or_else(|_| panic!("#{id} has wrong type"))
}

fn now_ms() -> f64 {
    js_sys::Date::now()
}

fn log(app: &App, msg: &str) {
    web_sys::console::log_1(&msg.into());
    let st = app.borrow();
    let line = st.ui.doc.create_element("div").unwrap();
    let time = js_sys::Date::new_0().to_locale_time_string("en-GB");
    line.set_text_content(Some(&format!("{} {msg}", String::from(time))));
    let _ = st.ui.log.append_child(&line);
    st.ui.log.set_scroll_top(st.ui.log.scroll_height().into());
}

/// Unfold the log, which is collapsed by default, so that an error shows.
fn open_log(app: &App) {
    let st = app.borrow();
    let _ = st.ui.log_panel.set_attribute("open", "");
    st.ui.log.set_scroll_top(st.ui.log.scroll_height().into());
}

fn set_phase(app: &App, phase: Phase, status: &str) {
    let mut st = app.borrow_mut();
    st.phase = phase;
    st.ui.status.set_text_content(Some(status));
    st.ui.status.set_class_name(match phase {
        Phase::Disconnected => "status off",
        Phase::Busy => "status busy",
        Phase::Ready => "status ready",
        Phase::Streaming => "status live",
    });
    st.ui.connect.set_disabled(phase != Phase::Disconnected);
    st.ui.start.set_disabled(phase != Phase::Ready);
    st.ui.diag.set_disabled(phase != Phase::Ready);
    st.ui.stop.set_disabled(phase != Phase::Streaming);
    st.ui.resolution.set_disabled(phase == Phase::Streaming);
    let _ = st.ui.controls.set_attribute(
        "data-enabled",
        if phase == Phase::Streaming { "1" } else { "0" },
    );
}

// ---------------------------------------------------------------- firmware

fn show_firmware(app: &App) {
    let st = app.borrow();
    let text = match &st.firmware {
        Some(fw) if fw.matches_reference && CONFIRMED_AGAINST_VENDOR_MEDIA => {
            format!("Loaded and verified against original vendor media · {}", fw.description())
        }
        Some(fw) if fw.matches_reference => format!(
            "Loaded · matches the kswit reference image (not yet confirmed against an original CD) · {}",
            fw.description()
        ),
        Some(fw) => format!(
            "Loaded · differs from the kswit reference (sha256 {}…) · {}",
            &fw.sha256[..12],
            fw.description()
        ),
        None => "Not loaded. Needed the first time the microscope is plugged in.".into(),
    };
    st.ui.fw_status.set_text_content(Some(&text));
    st.ui.fw_status.set_class_name(match &st.firmware {
        Some(fw) if fw.matches_reference => "fw ok",
        Some(_) => "fw warn",
        None => "fw missing",
    });
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

fn load_cached_firmware(app: &App) {
    let hex = storage().and_then(|s| s.get_item(FW_STORAGE_KEY).ok().flatten());
    if let Some(hex) = hex {
        let bytes: Option<Vec<u8>> = (0..hex.len())
            .step_by(2)
            .map(|i| {
                hex.get(i..i + 2)
                    .and_then(|b| u8::from_str_radix(b, 16).ok())
            })
            .collect();
        if let Some(fw) = bytes.and_then(|b| Firmware::extract(&b).ok()) {
            app.borrow_mut().firmware = Some(fw);
        }
    }
    show_firmware(app);
}

async fn on_firmware_file(app: App, input: HtmlInputElement) {
    let Some(file) = input.files().and_then(|f| f.get(0)) else {
        return;
    };
    let name = file.name();
    let buf = match file.array_buffer().await {
        Ok(b) => b,
        Err(e) => return log(&app, &format!("Could not read {name}: {e:?}")),
    };
    let bytes = js_sys::Uint8Array::new(&buf).to_vec();
    match Firmware::extract(&bytes) {
        Ok(fw) => {
            log(
                &app,
                &format!("Firmware found in {name} ({} bytes)", fw.bytes().len()),
            );
            if let Some(s) = storage() {
                let hex: String = fw.bytes().iter().map(|b| format!("{b:02x}")).collect();
                let _ = s.set_item(FW_STORAGE_KEY, &hex);
            }
            app.borrow_mut().firmware = Some(fw);
        }
        Err(e) => log(&app, &format!("{name}: {e}")),
    }
    input.set_value("");
    show_firmware(&app);
}

// ---------------------------------------------------------------- device

/// `known`: a device the page already has permission for (found at
/// startup); otherwise the browser's picker is shown.
async fn connect(app: App, known: Option<UsbDevice>) {
    set_phase(&app, Phase::Busy, "Connecting…");
    let result = try_connect(&app, known).await;
    app.borrow_mut().reconnecting = false;
    match result {
        Ok(Some((dev, transport))) => {
            {
                let mut st = app.borrow_mut();
                st.device = Some(dev);
                st.transport = transport;
            }
            set_phase(&app, Phase::Ready, "Ready");
        }
        Ok(None) => {
            log(
                &app,
                "The microscope has restarted with its bulk video endpoint. The browser \
                 treats it as a new device, so press Connect and choose it once more.",
            );
            set_phase(&app, Phase::Disconnected, "Press Connect again");
        }
        Err(e) => {
            log(&app, &format!("Connect failed: {e}"));
            open_log(&app);
            set_phase(&app, Phase::Disconnected, "Not connected");
        }
    }
}

/// Whether the page address contains `word`, as in `?bulk` or `?diag`.
fn query_has(word: &str) -> bool {
    web_sys::window()
        .and_then(|w| w.location().search().ok())
        .is_some_and(|q| q.contains(word))
}

/// Use the bulk endpoint where isochronous transfers are unavailable
/// (Chromium on Windows), or anywhere with `?bulk` in the page address.
fn want_bulk() -> bool {
    is_windows() || query_has("bulk")
}

/// `Ok(None)`: the device reconnected for bulk video, but the browser forgot
/// its permission (it has no serial number), so the user must pick it again.
async fn try_connect(
    app: &App,
    known: Option<UsbDevice>,
) -> Result<Option<(UsbDevice, Transport)>, String> {
    let dev = match known {
        Some(dev) => dev,
        None => usb::request_device().await?,
    };
    log(
        app,
        &format!(
            "Opening {}",
            dev.product_name().unwrap_or_else(|| "device".into())
        ),
    );
    usb::open(&dev).await?;
    for line in diag::describe(&dev) {
        log(app, &line);
    }
    if usb::has_bulk_video(&dev) {
        log(app, "Bulk-video firmware already running");
        return Ok(Some((dev, Transport::Bulk)));
    }

    // The boot ROM may answer the version request with the same string as the
    // firmware (kswit's tested kernel driver uploads even when it matches), so
    // the version cannot tell us whether firmware is loaded. Upload once per
    // plug-in; the same UsbDevice object persists until it is unplugged.
    let before = usb::read_version(&dev).await.unwrap_or_default();
    log(
        app,
        &format!("Device reports {:?}", String::from_utf8_lossy(&before)),
    );
    let bulk = want_bulk();
    if !bulk && app.borrow().fw_uploaded_to.as_ref() == Some(&dev) {
        log(
            app,
            "Firmware already uploaded since this device was plugged in",
        );
        return Ok(Some((dev, Transport::Iso)));
    }

    let fw = app
        .borrow()
        .firmware
        .clone()
        .ok_or("the microscope needs its firmware; load it from the CD first")?;
    // Add the code-memory reader used by Diagnostics. It only takes over the
    // unused request 0xA0, so normal operation is unaffected.
    let mut image = match scope_core::patch::with_code_reader(fw.bytes()) {
        Some(patched) => patched,
        None => fw.bytes().to_vec(),
    };
    if bulk {
        image = scope_core::patch::with_bulk_video(&image)
            .ok_or("this firmware image cannot be patched for bulk video, which Windows needs")?;
        log(app, "Uploading firmware (patched for bulk video)…");
    } else {
        log(app, "Uploading firmware…");
    }
    usb::upload_firmware(&dev, &image).await?;
    app.borrow_mut().fw_uploaded_to = Some(dev.clone());
    let version = usb::read_version(&dev).await?;
    if version.as_slice() != protocol::FIRMWARE_VERSION {
        return Err(format!(
            "unexpected firmware version {:?}",
            String::from_utf8_lossy(&version)
        ));
    }
    log(
        app,
        &format!("Firmware running: {}", String::from_utf8_lossy(&version)),
    );
    if !bulk {
        return Ok(Some((dev, Transport::Iso)));
    }

    // The host still holds the boot ROM's descriptors, which describe only
    // isochronous endpoints. The firmware drops off the bus and comes back,
    // and the host then reads its bulk descriptor.
    log(
        app,
        "Reconnecting so the computer sees the bulk video endpoint…",
    );
    app.borrow_mut().reconnecting = true;
    if let Err(e) = usb::request_reconnect(&dev).await {
        log(app, &format!("Reconnect request: {e} (waiting anyway)"));
    }
    // Chrome forgets a device without a serial number when it disconnects,
    // so the reconnected microscope needs the picker again. Show it at once,
    // while the Connect click still counts as recent (the browser allows a
    // few seconds); the picker lists the microscope as soon as it is back.
    usb::sleep(300).await;
    log(app, "Choose the microscope again to finish connecting");
    let dev = match usb::request_device().await {
        Ok(dev) if usb::has_bulk_video(&dev) => Some(dev),
        Ok(_) => None,
        // No longer allowed without a click, or cancelled: in case this
        // browser kept the permission after all, look for the device.
        Err(_) => usb::wait_for_bulk_device(RECONNECT_WAIT_MS).await,
    };
    match dev {
        Some(dev) => {
            usb::open(&dev).await?;
            log(app, "Reconnected with the bulk video endpoint");
            Ok(Some((dev, Transport::Bulk)))
        }
        None => Ok(None),
    }
}

async fn start(app: App) {
    let Some(dev) = app.borrow().device.clone() else {
        return;
    };
    set_phase(&app, Phase::Busy, "Starting…");
    let resolution = match app.borrow().ui.resolution.value().as_str() {
        "sxga" => Resolution::SXGA,
        _ => Resolution::VGA,
    };
    let transport = app.borrow().transport;
    let bulk = transport == Transport::Bulk;
    if let Err(e) = usb::start_stream(&dev, resolution, bulk).await {
        log(&app, &format!("Start failed: {e}"));
        open_log(&app);
        set_phase(&app, Phase::Ready, "Ready");
        return;
    }

    let generation = {
        let mut st = app.borrow_mut();
        st.generation += 1;
        st.resolution = resolution;
        st.assembler = if bulk {
            FrameAssembler::with_header_len(resolution.frame_bytes(), BULK_HEADER_LEN)
        } else {
            FrameAssembler::new(resolution.frame_bytes())
        };
        st.rgba = vec![0; resolution.width as usize * resolution.height as usize * 4];
        st.frame_times.clear();
        st.ui.canvas.set_width(resolution.width as u32);
        st.ui.canvas.set_height(resolution.height as u32);
        st.generation
    };
    set_phase(&app, Phase::Streaming, "Live");
    log(
        &app,
        &format!(
            "Streaming {}×{} over {}. Auto exposure can take a while to settle.",
            resolution.width,
            resolution.height,
            if bulk { "bulk" } else { "isochronous" }
        ),
    );
    if bulk {
        for _ in 0..BULK_TRANSFERS_IN_FLIGHT {
            spawn_local(bulk_loop(app.clone(), dev.clone(), generation));
        }
    } else {
        for _ in 0..TRANSFERS_IN_FLIGHT {
            spawn_local(iso_loop(app.clone(), dev.clone(), generation));
        }
    }
}

fn is_windows() -> bool {
    web_sys::window()
        .and_then(|w| w.navigator().user_agent().ok())
        .is_some_and(|ua| ua.contains("Windows"))
}

fn is_current(app: &App, generation: u32) -> bool {
    let st = app.borrow();
    st.phase == Phase::Streaming && st.generation == generation
}

async fn iso_loop(app: App, dev: UsbDevice, generation: u32) {
    let lengths = usb::packet_lengths(PACKETS_PER_TRANSFER);
    while is_current(&app, generation) {
        match usb::iso_in(&dev, &lengths).await {
            Ok(packets) => {
                if is_current(&app, generation) {
                    handle_packets(&app, &packets);
                }
            }
            Err(e) => {
                if is_current(&app, generation) {
                    log(&app, &format!("Stream error: {e}"));
                    open_log(&app);
                    if is_windows() {
                        log(
                            &app,
                            "Chrome and Edge on Windows do not implement isochronous USB \
                             transfers. Unplug the microscope, plug it back in and connect \
                             again; the page then uses the bulk endpoint instead.",
                        );
                    }
                    spawn_local(stop(app.clone()));
                }
                return;
            }
        }
    }
}

async fn bulk_loop(app: App, dev: UsbDevice, generation: u32) {
    while is_current(&app, generation) {
        match usb::bulk_in(&dev, BULK_TRANSFER_LEN).await {
            Ok(data) => {
                if is_current(&app, generation) {
                    handle_packets(&app, &[data]);
                }
            }
            Err(e) => {
                if is_current(&app, generation) {
                    log(&app, &format!("Stream error: {e}"));
                    open_log(&app);
                    spawn_local(stop(app.clone()));
                }
                return;
            }
        }
    }
}

fn handle_packets(app: &App, packets: &[Vec<u8>]) {
    let st = &mut *app.borrow_mut();
    let State {
        assembler,
        rgba,
        ui,
        order,
        resolution,
        frame_times,
        ..
    } = st;
    let mut drew = false;
    for p in packets {
        assembler.push(p, |frame| {
            yuv422_to_rgba(frame, rgba, *order);
            drew = true;
        });
    }
    if drew {
        if let Ok(img) = ImageData::new_with_u8_clamped_array_and_sh(
            Clamped(rgba),
            resolution.width as u32,
            resolution.height as u32,
        ) {
            let _ = ui.ctx.put_image_data(&img, 0, 0);
        }
        let t = now_ms();
        frame_times.push_back(t);
        while frame_times.front().is_some_and(|&f| t - f > 2000.0) {
            frame_times.pop_front();
        }
        let s = assembler.stats;
        ui.stats.set_text_content(Some(&format!(
            "{:.1} fps · {} frames · {} dropped · {:.1} MB received",
            frame_times.len() as f64 / 2.0,
            s.frames,
            s.dropped,
            s.bytes as f64 / 1e6
        )));
    }
}

async fn stop(app: App) {
    let Some(dev) = app.borrow().device.clone() else {
        return;
    };
    if app.borrow().phase != Phase::Streaming {
        return;
    }
    set_phase(&app, Phase::Busy, "Stopping…");
    usb::sleep(50).await; // let in-flight transfers drain
    let bulk = app.borrow().transport == Transport::Bulk;
    if let Err(e) = usb::stop_stream(&dev, bulk).await {
        log(&app, &format!("Stop: {e}"));
    }
    set_phase(&app, Phase::Ready, "Ready");
}

fn on_disconnect(app: &App, dev: &UsbDevice) {
    let ours = app.borrow().device.as_ref().is_some_and(|d| d == dev);
    if app.borrow().fw_uploaded_to.as_ref() == Some(dev) {
        app.borrow_mut().fw_uploaded_to = None;
    }
    if app.borrow().reconnecting {
        // Expected: the firmware is re-enumerating with its bulk endpoint.
        return;
    }
    if ours {
        app.borrow_mut().device = None;
        log(
            app,
            "Microscope unplugged. Plug it back in and connect again.",
        );
        set_phase(app, Phase::Disconnected, "Not connected");
    }
}

fn send_control(app: &App, control: Control, value: u16) {
    let Some(dev) = app.borrow().device.clone() else {
        return;
    };
    if app.borrow().phase != Phase::Streaming {
        return;
    }
    let app = app.clone();
    spawn_local(async move {
        if let Err(e) = usb::ctrl_out(&dev, &control.request(value)).await {
            log(&app, &format!("{}: {e}", control.label()));
        }
    });
}

async fn diagnostics(app: App) {
    let Some(dev) = app.borrow().device.clone() else {
        return;
    };
    set_phase(&app, Phase::Busy, "Running diagnostics…");
    for line in diag::describe(&dev) {
        log(&app, &line);
    }
    log(
        &app,
        "Reading the bridge chip's ROM, one byte at a time (a few minutes)…",
    );
    let result = diag::dump(&dev, |msg| log(&app, msg)).await;
    match result {
        Ok(bytes) => {
            log(
                &app,
                &format!("Done: {} bytes. Saving aveo-rom-dump.bin", bytes.len()),
            );
            save_file(&app, "aveo-rom-dump.bin", &bytes);
        }
        Err(e) => log(&app, &format!("Diagnostics stopped: {e}")),
    }
    set_phase(&app, Phase::Ready, "Ready");
}

fn save_file(app: &App, name: &str, bytes: &[u8]) {
    let parts = js_sys::Array::new();
    parts.push(&js_sys::Uint8Array::new_from_slice(bytes));
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type("application/octet-stream");
    let Ok(blob) = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &opts) else {
        return log(app, "Could not create the file");
    };
    let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) else {
        return;
    };
    let a: HtmlAnchorElement = app
        .borrow()
        .ui
        .doc
        .create_element("a")
        .unwrap()
        .unchecked_into();
    a.set_href(&url);
    a.set_download(name);
    a.click();
    let _ = web_sys::Url::revoke_object_url(&url);
}

fn snapshot(app: &App) {
    let st = app.borrow();
    let Ok(url) = st.ui.canvas.to_data_url_with_type("image/png") else {
        return;
    };
    let d = js_sys::Date::new_0();
    let name = format!(
        "microscope-{:04}{:02}{:02}-{:02}{:02}{:02}.png",
        d.get_full_year(),
        d.get_month() + 1,
        d.get_date(),
        d.get_hours(),
        d.get_minutes(),
        d.get_seconds()
    );
    let a: HtmlAnchorElement = st.ui.doc.create_element("a").unwrap().unchecked_into();
    a.set_href(&url);
    a.set_download(&name);
    a.click();
}

// ---------------------------------------------------------------- wiring

fn on<F: FnMut(web_sys::Event) + 'static>(target: &web_sys::EventTarget, event: &str, f: F) {
    let cb = Closure::<dyn FnMut(web_sys::Event)>::new(f);
    target
        .add_event_listener_with_callback(event, cb.as_ref().unchecked_ref())
        .unwrap();
    cb.forget();
}

fn build_controls(app: &App) {
    let (doc, container) = {
        let st = app.borrow();
        (st.ui.doc.clone(), st.ui.controls.clone())
    };
    for control in Control::ALL {
        let (min, max, default) = control.range();
        let row = doc.create_element("label").unwrap();
        row.set_class_name("control");
        let name = doc.create_element("span").unwrap();
        name.set_text_content(Some(control.label()));
        let input: HtmlInputElement = doc.create_element("input").unwrap().unchecked_into();
        input.set_id(control.key());
        if control.is_toggle() {
            input.set_type("checkbox");
            input.set_checked(default != 0);
        } else {
            input.set_type("range");
            input.set_min(&min.to_string());
            input.set_max(&max.to_string());
            input.set_value(&default.to_string());
        }
        let value = doc.create_element("output").unwrap();
        if !control.is_toggle() {
            value.set_text_content(Some(&default.to_string()));
        }
        row.append_with_node_3(&name, &input, &value).unwrap();
        container.append_child(&row).unwrap();

        let (a, i, v) = (app.clone(), input.clone(), value.clone());
        on(&input, "input", move |_| {
            if !control.is_toggle() {
                v.set_text_content(Some(&i.value()));
            }
        });
        let i = input.clone();
        on(&input, "change", move |_| {
            let value = if control.is_toggle() {
                i.checked() as u16
            } else {
                i.value().parse().unwrap_or(default)
            };
            send_control(&a, control, value);
        });
    }
}

fn main() {
    console_error_panic_hook::set_once();
    let window = web_sys::window().unwrap();
    let doc = window.document().unwrap();

    let canvas: HtmlCanvasElement = el(&doc, "view");
    let ctx: CanvasRenderingContext2d = canvas.get_context("2d").unwrap().unwrap().unchecked_into();
    let ui = Ui {
        log: el(&doc, "log"),
        log_panel: el(&doc, "log-panel"),
        status: el(&doc, "status"),
        fw_status: el(&doc, "fw-status"),
        stats: el(&doc, "stats"),
        connect: el(&doc, "connect"),
        start: el(&doc, "start"),
        stop: el(&doc, "stop"),
        snapshot: el(&doc, "snapshot"),
        diag: el(&doc, "diag"),
        resolution: el(&doc, "resolution"),
        order: el(&doc, "order"),
        controls: el(&doc, "controls"),
        canvas,
        ctx,
        doc: doc.clone(),
    };
    let app: App = Rc::new(RefCell::new(State {
        ui,
        phase: Phase::Disconnected,
        firmware: None,
        fw_uploaded_to: None,
        device: None,
        transport: Transport::Iso,
        reconnecting: false,
        generation: 0,
        assembler: FrameAssembler::new(Resolution::VGA.frame_bytes()),
        rgba: Vec::new(),
        resolution: Resolution::VGA,
        order: ByteOrder::Uyvy,
        frame_times: VecDeque::new(),
    }));

    if !usb::supported() {
        let banner: HtmlElement = el(&doc, "unsupported");
        banner.set_hidden(false);
        set_phase(&app, Phase::Disconnected, "WebUSB unavailable");
        app.borrow().ui.connect.set_disabled(true);
    } else {
        set_phase(&app, Phase::Disconnected, "Not connected");
        let a = app.clone();
        usb::on_disconnect(move |dev| on_disconnect(&a, &dev));
    }

    load_cached_firmware(&app);
    build_controls(&app);

    let fw_input: HtmlInputElement = el(&doc, "fw-file");
    let a = app.clone();
    let input = fw_input.clone();
    on(&fw_input, "change", move |_| {
        spawn_local(on_firmware_file(a.clone(), input.clone()))
    });

    let st = app.borrow();
    let a = app.clone();
    on(&st.ui.connect, "click", move |_| {
        spawn_local(connect(a.clone(), None))
    });
    let a = app.clone();
    on(&st.ui.start, "click", move |_| {
        spawn_local(start(a.clone()))
    });
    let a = app.clone();
    on(&st.ui.stop, "click", move |_| spawn_local(stop(a.clone())));
    let a = app.clone();
    on(&st.ui.snapshot, "click", move |_| snapshot(&a));
    // The ROM dump is a development tool: shown only with `?diag`.
    st.ui.diag.set_hidden(!query_has("diag"));
    let a = app.clone();
    on(&st.ui.diag, "click", move |_| {
        spawn_local(diagnostics(a.clone()))
    });
    let a = app.clone();
    let order = st.ui.order.clone();
    on(&st.ui.order, "change", move |_| {
        a.borrow_mut().order = if order.value() == "yuyv" {
            ByteOrder::Yuyv
        } else {
            ByteOrder::Uyvy
        };
    });
    drop(st);

    log(&app, "Ready. Plug in the microscope and press Connect.");
    if usb::supported() {
        spawn_local(auto_connect(app.clone()));
    }
}

/// Connect without a click when the browser still allows this page to use
/// the microscope, as after a reload while it stays plugged in.
async fn auto_connect(app: App) {
    let Some(dev) = usb::known_device().await else {
        return;
    };
    if app.borrow().phase == Phase::Disconnected {
        log(&app, "Microscope already permitted; connecting");
        connect(app, Some(dev)).await;
    }
}
