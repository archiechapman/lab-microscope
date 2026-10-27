# Lab Microscope

A browser viewer for the AVEO Cheetah3 USB microscope (USB ID `1871:01b0`), sold as the **Traveller 6803** and under other brands. Specifically, it replaces the original Windows software, which modern Windows versions refuse to run, with a page that talks to the microscope directly over WebUSB. Everything is written in Rust and compiled to WebAssembly.

## Using it

1. Open the hosted page in a Chromium browser (Chrome, Edge, Brave) on Windows, a Chromebook, Linux or macOS. Firefox and Safari have no WebUSB.
2. **Windows only, once:** install [Zadig](https://zadig.akeo.ie/), choose *Options → List All Devices*, select `1871 01B0`, pick **WinUSB** and press *Replace Driver*. On Linux, add a udev rule for `1871:01b0`; macOS needs nothing extra.
3. **Load the firmware, once:** choose a driver file from the original CD. The page searches it for the firmware image and remembers it in this browser only.
4. Press **Connect**, select the microscope, then **Start**. On Windows the microscope restarts itself after the firmware upload, and the browser then sees it as a new device: press **Connect** and select it a second time.

> **Why Windows is different:** Chromium's WebUSB on Windows does not implement isochronous transfers, which the microscope uses for video. On Windows the page therefore patches the firmware, in memory, to stream over a bulk endpoint instead. The boot ROM's USB descriptors describe only the isochronous endpoint, so after the upload the firmware detaches from the bus and reattaches with a descriptor of its own, and the computer enumerates it afresh. Add `?bulk` to the page address to use the bulk route on other systems too.

## Why the firmware step exists

The microscope's bridge chip is an 8051 microcontroller with no non-volatile memory, so the host must upload 4,928 bytes of firmware every time it is plugged in. However, that firmware is AVEO's proprietary code. Against this background, this project ships no copy of it; each user supplies it from their own CD. In particular, the loader accepts:

- a vendor driver or application file that embeds the image,
- a raw 4,928-byte dump, or
- a C header listing the bytes as `0xNN` literals.

The page reports whether an image matches the reference SHA-256 from kswit's published copy. Note that this reference has yet to be confirmed against an original CD, so a match means "identical to kswit's copy" rather than "genuine". Loading a driver file from an original CD is the real test: if it matches, the reference is confirmed. Moreover, an open replacement firmware is a planned later phase, after which the CD becomes optional.

## How it works

```
[Lens + LED] → [Aptina MT9M111 sensor] → [AVEO Cheetah 8051 bridge] → USB 2.0 → WebUSB → Rust/WASM → <canvas>
```

| Step | Transfer | Detail |
|------|----------|--------|
| Firmware | vendor OUT `0x04` × 154 | 32-byte chunks, `wIndex` = chunk number |
| Boot | vendor OUT `0x05`, `wIndex=1` | then wait 500 ms |
| Verify | vendor IN `0x08`, 12 bytes | expects `Ver R003.001` |
| Stream | interface 0, alt setting 5 | isochronous endpoint 0x83, 3 × 1024 B per microframe |
| Resolution | vendor OUT `0x32` | `wValue`=width, `wIndex`=`0x1000` \| height |
| Capture | vendor OUT `0x22` | `(1, 2)` start, `(0, 0)` stop |
| Image | vendor OUT `0x5c`–`0x73` | exposure, gain, brightness, contrast, sharpness, saturation |

Frames begin with the marker `00 FF FF FF` and a header (10 bytes on the isochronous endpoint per kswit's driver, 6 bytes measured on the bulk endpoint), followed by packed UYVY 4:2:2 pixels. Within this context, 640 × 480 at 30 fps fits the isochronous bandwidth, while full 1280 × 1024 does not and is offered only as an experiment.

All commands are sent with the *device* recipient. In contrast to the interface and endpoint recipients, WebUSB does not check `wIndex` for device requests, which matters because the firmware chunk numbers would otherwise be rejected.

## Layout

| Path | Contents |
|------|----------|
| `crates/scope-core` | Pure Rust, no I/O: protocol constants and sequences, frame reassembly, YUV→RGBA, firmware search and verification. Unit tested. |
| `crates/scope-web` | WebUSB front end built with `wasm-bindgen` and `web-sys`, plus the page itself. |
| `.github/workflows/pages.yml` | Tests the core, builds with Trunk and deploys to GitHub Pages. |

## Building locally

```sh
rustup target add wasm32-unknown-unknown
cargo install trunk
cargo test -p scope-core
cd crates/scope-web && trunk serve
```

Then open <http://localhost:8080>. WebUSB works on `localhost` without HTTPS. Note that `.cargo/config.toml` sets `--cfg=web_sys_unstable_apis` for the WASM target, which the WebUSB bindings require.

To check the firmware loader against a reference image, run `AVEO_FW=/path/to/file cargo test -p scope-core reference`.

## Deploying

Push to `main` on GitHub, then set *Settings → Pages → Source* to **GitHub Actions**. The workflow publishes to `https://<user>.github.io/<repo>/`.

## Known limitations

- On Windows, Connect must be pressed twice (the browser forgets the microscope when it reconnects for bulk video, because it has no serial number). The bulk route has been tested at 640 × 480 only.
- Auto exposure starts dark and can take up to half a minute to settle.
- Control ranges are inferred from the vendor defaults and have yet to be confirmed.
- The LED is controlled by the physical switch on the microscope.
- On some AMD USB controllers the device can drop off the bus; unplug and replug it.

## Credits and licence

The protocol was reverse engineered by **kswit** ([kswit/USB_driver](https://github.com/kswit/USB_driver), MIT). This project is released under the MIT licence; see `LICENSE` and `NOTICE`.
