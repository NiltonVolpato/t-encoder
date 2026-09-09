# espielberg

> *"Lights, camera, action!"*

`espielberg` is a host-side Rust driver library and Model Context Protocol (MCP) server for inspecting, testing, and directing the LilyGo T-Encoder-Pro (ESP32-S3) device.

---

## The Name & Concept

`espielberg` is named after:
- **ESP**: The ESP32-S3 microcontroller powering the device.
- **Steven Spielberg**: The visionary film director.

Just as *Playwright* controls web browsers through the metaphor of theater, `espielberg` directs embedded hardware devices through the metaphor of a **movie set**.

---

## Movie Set Terminology & API

The entire API and MCP tool interface is built around movie set terminology:

| Movie Term | API / Tool | Purpose |
|---|---|---|
| **Director** | `Director` | The main controller holding the connection to the physical stage. |
| **"Action!"** | `Director::action(port)` / `action { port }` | Enters the set and opens the serial connection to the specified port. Errors if already in action. |
| **"Take"** | `director.take()` / `take { filename? }` | Captures a live screenshot of the round AMOLED display and saves the shot to disk. Errors if not in action. |
| **Shot** | `Shot` | The captured frame ($390\times390$), holding raw RGB565-BE bytes with helpers to save `.raw` or `.png`. |
| **"Cut!"** | `director.cut()` / `cut {}` | Ends the shoot, cleanly closes the serial connection, and frees the hardware port for flashing or terminal monitoring. |
| **"Cue"** | `director.cue(Cue)` / `cue { ... }` | Directs the stage with input events (`Cue::Tap { x, y }`, `Cue::Rotate(delta)`, `Cue::ShortPress`, `Cue::LongPress`, `Cue::Swipe`). |
| **"Reset"** | `director.reset()` / `reset {}` | Reboots the device set to initial boot state, waits for reboot, and reconnects. |

---

## Decoupled MCP Architecture

When running as an MCP server for AI coding assistants (such as Antigravity):
- The server runs persistently over `stdio` in the background.
- It **does not hold the serial port open** while idle.
- An AI assistant or test script calls `action { port: "/dev/cu.usbmodem101" }` when ready to interact with the device, performs one or more `take` calls, and calls `cut {}` when finished.
- This prevents serial port contention, allowing firmware to be flashed (`just flash`) or inspected with `tio` at any time without stopping the MCP server.

---

## Usage Example (Rust Library)

```rust
use espielberg::{Director, Shot};

fn main() -> anyhow::Result<()> {
    // Call "Action!" on the set
    let mut director = Director::action("/dev/cu.usbmodem101")?;

    // Shoot a take
    let shot = director.take()?;
    println!("Captured frame: {}x{}", shot.width(), shot.height());

    // Save shot as PNG and raw framebuffer
    shot.save_png("take_01.png")?;
    shot.save_raw("take_01.raw")?;

    // Call "Cut!" to release the stage
    director.cut()?;

    Ok(())
}
```

---

## Usage Example (MCP Server)

Run `espielberg` as an MCP server over stdio:

```bash
cargo run -p espielberg
```

Tools exposed:
- `action { port: string }`: Connects to device serial port.
- `cue { rotate?, press?, long_press?, tap?, swipe? }`: Injects dial, button, or touch events.
- `take { filename?: string }`: Captures screen and saves image to disk.
- `reset {}`: Reboots the device and reconnects.
- `cut {}`: Disconnects and releases serial port.
