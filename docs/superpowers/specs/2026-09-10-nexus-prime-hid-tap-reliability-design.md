# Nexus Prime HID Tap Reliability Design

**Date:** 2026-09-10

**Target version:** 0.4.2
**Status:** Approved for implementation

## Context

Nexus Prime 0.4.1 injects a Frida Gadget into the RC003 `WUDFHost.exe` and hooks
`NtDeviceIoControlFile` to forward HID-over-GATT reports. The current hook emits a
report only when the call returns immediate `STATUS_SUCCESS`. It does not retain
the `IO_STATUS_BLOCK` or output buffer when the call returns `STATUS_PENDING`.

Two live repair cycles on 2026-09-10 fully stopped and recreated the Rust HID Tap
hub, attached to the same device host, reached `io_verified=true`, and forwarded
several key/voice events. Within seconds, device-correlated HID reports stopped
while the TCP heartbeat and native Windows key observation continued. This rules
out a dead application process, lost Bluetooth pairing, or a mapping-file error.

The injected 0.4.1 Gadget also uses `on_change: ignore`, so an already-loaded
Gadget will not consume the new script until the RC003 WUDF host is restarted.
The 0.4.2 installation procedure therefore requires one Bluetooth off/on cycle
or a Windows restart after installation.

## Goals

- Capture both immediate and asynchronous HID IOCTL completions exactly once.
- Preserve the nine-byte RC003 report validation before forwarding data.
- Make the user-facing key-bridge restart rebuild the Rust HID Tap hub.
- Make future Gadget script updates reloadable without changing user mappings.
- Preserve the existing ATVV, virtual keyboard, audio, and configuration behavior.
- Produce a signed-state-independent NSIS installer named as version 0.4.2.

## Non-Goals

- Replacing HID Tap with Raw Input.
- Changing RC003 pairing, firmware, ATVV negotiation, or audio decoding.
- Automatically terminating or restarting `WUDFHost.exe`.
- Migrating or resetting the user's Xiaomi configuration.

## Considered Approaches

### 1. Restart-only mitigation

Force `stop_and_join()` from the key-bridge restart command. This improves the
button semantics but does not prevent another pending completion from being lost.
Rejected as an incomplete fix.

### 2. Async completion tracking plus a real hub restart

Retain pending IO contexts in the Gadget, sweep completed contexts before a new
target IOCTL reuses their memory, and also sweep them on a short timer. Rebuild the
Rust hub during a user-requested restart. Selected because it addresses the observed
failure at its source and retains the proven HID report decoder.

### 3. Raw Input replacement

Route all buttons through Raw Input and identify RC003 by device path. This has a
larger behavioral surface, and the current Raw Input bridge does not enforce the
device token when applying mappings. Rejected for this patch release.

## Detailed Design

### Gadget completion tracker

The hook records the `IO_STATUS_BLOCK`, output pointer, requested output length,
and a unique request id for every matching IOCTL.

Before each new matching `NtDeviceIoControlFile` call, the hook sweeps retained
contexts. This is important because WUDF can complete one request and immediately
reuse the same status block for the next read before a timer fires. A 10 ms timer
provides the fallback when no subsequent read is issued.

On return:

- `STATUS_SUCCESS`: read `IO_STATUS_BLOCK.Information`, validate that exactly nine
  bytes completed, emit once, and remove the context.
- `STATUS_PENDING`: retain the context until a sweep observes a terminal status.
- Any other terminal status: remove the context without reading the output buffer.

The tracker removes a context before emitting, catches invalid-memory reads, and
caps retained contexts so cancellation or a damaged caller cannot grow the map
without bound. Heartbeats include completion-tracker counters for diagnostics but
remain transport health signals rather than proof that a key report completed.

### Hub lifecycle

`restart_xiaomi_bridge_inner` will stop and join the process-level HID Tap hub while
holding the existing lifecycle lock, then stop/restart the BLE worker. The ATVV
repair path will use this shared restart behavior instead of stopping the hub before
the lock, preventing overlapping repair clicks from interleaving two stop/restart
sequences.

The Gadget config changes `on_change` from `ignore` to `reload`. This does not hot
upgrade the already-loaded 0.4.1 Gadget, but after the required one-time Bluetooth
or Windows restart it makes later script-only updates reloadable.

### Versioning and packaging

`package.json`, `package-lock.json`, `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock`,
and `src-tauri/tauri.conf.json` will report version 0.4.2. The build output is the
standard current-user NSIS installer and preserves `%APPDATA%` configuration/logs.

## Testing

A Node test harness evaluates the production Gadget script with mocked Frida pointer,
socket, timer, and interceptor primitives. It verifies:

- immediate success emits one nine-byte report;
- `STATUS_PENDING` emits nothing before completion;
- a later successful `IO_STATUS_BLOCK` completion emits exactly once;
- a failed/cancelled asynchronous completion emits no report;
- reuse of the same status block flushes the previous completion before replacement;
- the periodic sweep handles completion when no next IOCTL occurs.

Rust tests cover the restart sequencing helper and Gadget config/source contracts.
The final verification runs the focused regression tests, full `npm test`, frontend
build, full Cargo tests/checks, and `npm run tauri:build`.

## Acceptance Criteria

- The regression test fails against 0.4.1 behavior and passes after the fix.
- The NSIS installer builds successfully as version 0.4.2.
- Existing Xiaomi mappings remain unchanged after an upgrade installation.
- After installing and cycling Bluetooth once, at least 30 alternating and repeated
  remote button presses continue to produce device-correlated HID Tap reports.
- “Restart key bridge” logs a complete HID Tap stop/start cycle instead of singleton
  reuse.
