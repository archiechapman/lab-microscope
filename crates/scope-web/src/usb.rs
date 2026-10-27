//! Thin async wrappers over WebUSB for this device.

use scope_core::protocol::{self, req, Request, Resolution};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{
    UsbControlTransferParameters, UsbDevice, UsbDeviceFilter, UsbDeviceRequestOptions,
    UsbDirection, UsbEndpointType, UsbRecipient, UsbRequestType, UsbTransferStatus,
};

fn err(e: JsValue) -> String {
    e.dyn_ref::<js_sys::Error>()
        .map(|e| String::from(e.message()))
        .unwrap_or_else(|| format!("{e:?}"))
}

pub fn supported() -> bool {
    web_sys::window()
        .map(|w| js_sys::Reflect::has(&w.navigator(), &"usb".into()).unwrap_or(false))
        .unwrap_or(false)
}

fn usb() -> web_sys::Usb {
    web_sys::window().unwrap().navigator().usb()
}

pub async fn sleep(ms: u32) {
    if ms == 0 {
        return;
    }
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        web_sys::window()
            .unwrap()
            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms as i32)
            .unwrap();
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

/// Show the browser's device picker, filtered to the microscope.
pub async fn request_device() -> Result<UsbDevice, String> {
    let filter = UsbDeviceFilter::new();
    filter.set_vendor_id(protocol::VENDOR_ID);
    filter.set_product_id(protocol::PRODUCT_ID);
    let opts = UsbDeviceRequestOptions::new(&[filter]);
    usb().request_device(&opts).await.map_err(err)
}

/// Whether the active configuration has bulk IN endpoint 3, i.e. the
/// bulk-video firmware is running and the host has enumerated it.
pub fn has_bulk_video(dev: &UsbDevice) -> bool {
    let Some(cfg) = dev.configuration() else {
        return false;
    };
    cfg.interfaces().iter().any(|iface| {
        iface.alternates().iter().any(|alt| {
            alt.endpoints().iter().any(|e| {
                e.endpoint_number() == protocol::ISO_ENDPOINT
                    && e.direction() == UsbDirection::In
                    && e.type_() == UsbEndpointType::Bulk
            })
        })
    })
}

fn is_microscope(dev: &UsbDevice) -> bool {
    dev.vendor_id() == protocol::VENDOR_ID && dev.product_id() == protocol::PRODUCT_ID
}

/// A microscope this page may use without showing the picker, if any.
pub async fn known_device() -> Option<UsbDevice> {
    let devices = usb().get_devices().await.ok()?;
    devices.iter().find(is_microscope)
}

/// After a reconnect, find the microscope again among the devices this page
/// may already use. Chrome may drop the permission for a device without a
/// serial number when it disconnects, in which case this times out.
pub async fn wait_for_bulk_device(timeout_ms: u32) -> Option<UsbDevice> {
    let mut waited = 0;
    while waited < timeout_ms {
        sleep(250).await;
        waited += 250;
        let Ok(devices) = usb().get_devices().await else {
            continue;
        };
        if let Some(dev) = devices
            .iter()
            .find(|d| is_microscope(d) && has_bulk_video(d))
        {
            return Some(dev);
        }
    }
    None
}

pub fn on_disconnect(mut f: impl FnMut(UsbDevice) + 'static) {
    let cb = Closure::<dyn FnMut(JsValue)>::new(move |ev: JsValue| {
        if let Ok(dev) = js_sys::Reflect::get(&ev, &"device".into()) {
            if let Ok(dev) = dev.dyn_into::<UsbDevice>() {
                f(dev);
            }
        }
    });
    usb()
        .add_event_listener_with_callback("disconnect", cb.as_ref().unchecked_ref())
        .unwrap();
    cb.forget();
}

pub async fn open(dev: &UsbDevice) -> Result<(), String> {
    if !dev.opened() {
        dev.open().await.map_err(err)?;
    }
    if dev.configuration().is_none() {
        dev.select_configuration(protocol::CONFIGURATION)
            .await
            .map_err(err)?;
    }
    dev.claim_interface(protocol::INTERFACE)
        .await
        .map_err(err)?;
    Ok(())
}

/// All commands use the *device* recipient; see `scope_core::protocol`.
fn setup(request: u8, value: u16, index: u16) -> UsbControlTransferParameters {
    UsbControlTransferParameters::new(
        index,
        UsbRecipient::Device,
        request,
        UsbRequestType::Vendor,
        value,
    )
}

pub async fn ctrl_out(dev: &UsbDevice, r: &Request) -> Result<(), String> {
    let res = dev
        .control_transfer_out(&setup(r.request, r.value, r.index))
        .await
        .map_err(err)?;
    if res.status() != UsbTransferStatus::Ok {
        return Err(format!(
            "request 0x{:02x} status {:?}",
            r.request,
            res.status()
        ));
    }
    sleep(r.delay_ms).await;
    Ok(())
}

async fn ctrl_out_data(
    dev: &UsbDevice,
    request: u8,
    value: u16,
    index: u16,
    data: &[u8],
) -> Result<(), String> {
    let buf = js_sys::Uint8Array::new_from_slice(data);
    let res = dev
        .control_transfer_out_with_buffer_source(&setup(request, value, index), &buf)
        .map_err(err)?
        .await
        .map_err(err)?;
    if res.status() != UsbTransferStatus::Ok || res.bytes_written() as usize != data.len() {
        return Err(format!(
            "request 0x{request:02x} index {index}: short or failed write"
        ));
    }
    Ok(())
}

pub async fn ctrl_in(
    dev: &UsbDevice,
    request: u8,
    value: u16,
    index: u16,
    len: u16,
) -> Result<Vec<u8>, String> {
    let res = dev
        .control_transfer_in(&setup(request, value, index), len)
        .await
        .map_err(err)?;
    if res.status() != UsbTransferStatus::Ok {
        return Err(format!("request 0x{request:02x} status {:?}", res.status()));
    }
    Ok(res.data().map(|dv| view_bytes(&dv)).unwrap_or_default())
}

fn view_bytes(dv: &js_sys::DataView) -> Vec<u8> {
    js_sys::Uint8Array::new_with_byte_offset_and_length(
        &dv.buffer(),
        dv.byte_offset() as u32,
        dv.byte_length() as u32,
    )
    .to_vec()
}

pub async fn read_version(dev: &UsbDevice) -> Result<Vec<u8>, String> {
    ctrl_in(dev, req::VERSION, 0, 0, 12).await
}

/// Upload and boot. The chunks go last to first: if firmware is already
/// running (say, after a page reload), a patched image then has its appended
/// code in place before the chunks that redirect the running interrupt
/// handler into it, and the vectors in chunk 0 change last.
pub async fn upload_firmware(dev: &UsbDevice, image: &[u8]) -> Result<(), String> {
    let chunks: Vec<&[u8]> = image.chunks_exact(protocol::FIRMWARE_CHUNK_LEN).collect();
    for (i, chunk) in chunks.iter().enumerate().rev() {
        ctrl_out_data(dev, req::FIRMWARE_CHUNK, 0, i as u16, chunk).await?;
    }
    ctrl_out(dev, &protocol::BOOT_REQUEST).await
}

/// Ask the bulk-video firmware to detach and reattach (see
/// `scope_core::patch::RECONNECT_REQUEST`). The device completes the request
/// before it detaches, but a failure here is not conclusive either way.
pub async fn request_reconnect(dev: &UsbDevice) -> Result<(), String> {
    ctrl_out(
        dev,
        &Request::new(scope_core::patch::RECONNECT_REQUEST, 0, 0, 0),
    )
    .await
}

/// `bulk`: the bulk-video firmware has a single alternate setting, so there
/// is no bandwidth to select.
pub async fn start_stream(
    dev: &UsbDevice,
    resolution: Resolution,
    bulk: bool,
) -> Result<(), String> {
    if !bulk {
        dev.select_alternate_interface(protocol::INTERFACE, protocol::STREAM_ALT_SETTING)
            .await
            .map_err(err)?;
    }
    for r in protocol::start_sequence(resolution) {
        ctrl_out(dev, &r).await?;
    }
    Ok(())
}

pub async fn stop_stream(dev: &UsbDevice, bulk: bool) -> Result<(), String> {
    for r in protocol::stop_sequence() {
        ctrl_out(dev, &r).await?;
    }
    if !bulk {
        dev.select_alternate_interface(protocol::INTERFACE, protocol::IDLE_ALT_SETTING)
            .await
            .map_err(err)?;
    }
    Ok(())
}

/// One bulk IN transfer from the video endpoint.
pub async fn bulk_in(dev: &UsbDevice, len: u32) -> Result<Vec<u8>, String> {
    let res = dev
        .transfer_in(protocol::ISO_ENDPOINT, len)
        .await
        .map_err(err)?;
    if res.status() != UsbTransferStatus::Ok {
        return Err(format!("bulk transfer status {:?}", res.status()));
    }
    Ok(res.data().map(|dv| view_bytes(&dv)).unwrap_or_default())
}

pub fn packet_lengths(n: usize) -> Vec<js_sys::Number> {
    (0..n)
        .map(|_| js_sys::Number::from(protocol::ISO_PACKET_LEN as f64))
        .collect()
}

/// One isochronous IN transfer. Returns the payload of each good packet.
pub async fn iso_in(dev: &UsbDevice, lengths: &[js_sys::Number]) -> Result<Vec<Vec<u8>>, String> {
    let res = dev
        .isochronous_transfer_in(protocol::ISO_ENDPOINT, lengths)
        .await
        .map_err(err)?;
    let packets = res.packets();
    let mut out = Vec::with_capacity(packets.length() as usize);
    for p in packets.iter() {
        if p.status() == UsbTransferStatus::Ok {
            if let Some(dv) = p.data() {
                if dv.byte_length() > 0 {
                    out.push(view_bytes(&dv));
                }
            }
        }
    }
    Ok(out)
}
