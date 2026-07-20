# bladestats

A lightweight, free, open-source, portable hardware monitor and FPS overlay for Windows 10/11
and Linux.

No installer, no service, no account, no telemetry. A single executable that draws your CPU,
GPU, memory and frame rate on top of a game and otherwise stays out of the way.

**Status: work in progress.** The overlay window and renderer are working; hardware telemetry
and FPS are not wired up yet.

## Design constraints

These are load-bearing, not preferences. Everything else in the project follows from them.

**On Windows, nothing is injected into the game.** No DLL is loaded into the target process, no
`Present` hook is installed, no foreign memory is read. Frame timing comes from ETW, the
tracing facility built into Windows. To an anti-cheat, bladestats is an ordinary window plus a
system trace.

The cost is that the overlay only works in borderless ("fullscreen windowed") mode. Drawing on
top of someone else's swapchain in exclusive fullscreen is not possible without a hook, so
bladestats detects that mode and hides instead.

**On Linux the mechanism is different.** There is no equivalent of ETW, so frame timing comes
from a Vulkan layer (`VK_LAYER_bladestats_overlay`). A Vulkan layer is code running inside the
game process. That is a real difference from the Windows side, and it is stated here rather
than glossed over.

**Overhead is the headline metric.** An overlay that costs frames defeats its own purpose, so
it is measured rather than assumed. On an idle desktop, release build:

| | Measured | Goal |
|---|---|---|
| CPU | 0.05% of one core | under 1% |
| Private memory | 36 MB | under 30 MB — **not met** |
| Binary | 1.2 MB | — |

The memory goal is currently missed. Most of the footprint is the graphics stack the overlay
has to load in order to draw at all — D3D11, DXGI and the display driver account for the bulk
of the 61 modules in the process. Whether that can be brought down without giving up
GPU-composited rendering is an open question, not a solved one.

## What it shows

- FPS, frame time, 1% and 0.1% lows — for both D3D and Vulkan titles
- CPU: load and clock **per core**, plus the exact model name
- GPU: load, VRAM, temperature, clocks, power draw, exact model name
- Memory: used/total and configured speed
- Power draw wherever a real sensor exists

### About power readings

| | Windows | Linux |
|---|---|---|
| GPU | yes (NVML; AMD/Intel later via ADLX/IGCL) | yes (hwmon) |
| CPU | **estimate**, always marked with a tilde: `~65 W` | yes (RAPL) |
| Memory | no | no |

CPU package power on Windows can only be read from MSRs, which requires a kernel-mode driver.
Drivers of that kind (WinRing0 and relatives) appear in Microsoft's vulnerable-driver blocklist
and are flagged by anti-cheats — that single component would carry more risk than the whole of
the rest of bladestats. So Windows shows a figure derived from load, clocks and TDP, and it is
always marked as an estimate.

Memory power is not shown at all. Consumer platforms have no power sensor for RAM, in SPD, in
SMBIOS or in hwmon. Rather than invent a plausible number, bladestats shows capacity, speed and
timings and leaves it at that.

The same principle runs through the whole UI: a metric that could not be read is drawn as a
dash, never as a zero.

## Building

```sh
cargo build --release
```

The font file is not stored in the repository — see
[assets/fonts/README.md](assets/fonts/README.md).

## Licence

Code is MIT, see [LICENSE](LICENSE).

JetBrains Mono is distributed under the SIL Open Font License 1.1. Its text lives next to the
font in [assets/fonts/LICENSE-JetBrainsMono.txt](assets/fonts/LICENSE-JetBrainsMono.txt) and
must accompany any build that embeds it.
