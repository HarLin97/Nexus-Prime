# Nexus Prime HID Tap Reliability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (- [ ]) syntax for tracking.

**Goal:** Build Nexus Prime 0.4.2 with reliable immediate and asynchronous RC003 HID completion capture, serialized HID Tap restarts, and an upgrade-safe NSIS installer.

**Architecture:** The production Frida Gadget keeps pending NtDeviceIoControlFile contexts keyed by IO_STATUS_BLOCK, flushes completed contexts before WUDF reuses their memory, and performs a short periodic sweep as a fallback. Rust owns restart serialization under the existing lifecycle lock and future Gadget script changes use Frida reload mode.

**Tech Stack:** JavaScript running in Frida Gadget/QJS, Node.js node:test and vm, Rust 2021, Tauri 2, Vue 3, NSIS.

**Spec:** docs/superpowers/specs/2026-09-10-nexus-prime-hid-tap-reliability-design.md

## Global Constraints

- Target application version is exactly 0.4.2.
- Preserve the existing Xiaomi JSON configuration and APPDATA logs.
- Do not replace HID Tap with Raw Input.
- Do not change RC003 pairing, ATVV negotiation, audio, or virtual-keyboard behavior.
- Do not terminate or restart WUDFHost automatically.
- The first installed 0.4.2 run requires one Bluetooth off/on cycle or Windows restart because 0.4.1 loaded its Gadget with on_change set to ignore.
- Every production behavior change follows a witnessed red-green regression cycle.

---

### Task 1: Add the executable Gadget regression harness

**Files:**
- Create: tests/xiaomi_hid_gadget.test.mjs
- Modify: package.json

**Interfaces:**
- Consumes: production script at src-tauri/src/bridges/xiaomi/xiaomi_hid_gadget.js
- Produces: npm run test:gadget and a VM harness exposing invoke(), sweep(), messages, and fake Frida pointers

- [ ] **Step 1: Add the Node test command**

Add this script without changing the application version yet:

~~~json
{
  "scripts": {
    "test:gadget": "node --test tests/xiaomi_hid_gadget.test.mjs",
    "test": "npm run test:gadget && vitest run"
  }
}
~~~

- [ ] **Step 2: Create the Frida VM harness and failing tests**

The test file loads the production script, captures Interceptor.attach, and models IO_STATUS_BLOCK.Status plus Information:

~~~javascript
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const READ_CHARACTERISTIC_IOCTL = 0x80018483;
const STATUS_SUCCESS = 0;
const STATUS_PENDING = 0x00000103;
const STATUS_CANCELLED = 0xc0000120;
const REPORT = [1, 0, 0, 0x28, 0, 0, 0, 0, 0];

function u32(value) {
  return { toUInt32: () => value >>> 0 };
}

class OutputPointer {
  constructor(bytes) { this.bytes = Uint8Array.from(bytes); }
  isNull() { return false; }
  readByteArray(length) { return this.bytes.slice(0, length).buffer; }
  toString() { return "[output]"; }
}

class StatusPointer {
  constructor(state, offset = 0) { this.state = state; this.offset = offset; }
  isNull() { return false; }
  add(offset) { return new StatusPointer(this.state, this.offset + offset); }
  readU32() {
    assert.equal(this.offset, 0);
    return this.state.status >>> 0;
  }
  readU64() {
    assert.equal(this.offset, 8);
    return { toNumber: () => this.state.information };
  }
  toString() { return "iosb:" + this.state.id; }
}

async function flushPromises() {
  for (let index = 0; index < 8; index += 1) await Promise.resolve();
}

async function loadGadget() {
  const sourceUrl = new URL(
    "../src-tauri/src/bridges/xiaomi/xiaomi_hid_gadget.js",
    import.meta.url
  );
  const source = readFileSync(sourceUrl, "utf8");
  const messages = [];
  const intervals = [];
  const timeouts = [];
  let hook = null;

  const socketOutput = {
    async writeAll(bytes) {
      const text = String.fromCharCode(...bytes);
      for (const line of text.split("\n")) {
        if (line) messages.push(JSON.parse(line));
      }
    }
  };

  const context = vm.createContext({
    Process: {
      id: 4242,
      pointerSize: 8,
      findModuleByName(name) {
        assert.equal(name, "ntdll.dll");
        return {
          findExportByName(symbol) {
            assert.equal(symbol, "NtDeviceIoControlFile");
            return { symbol };
          }
        };
      }
    },
    Socket: {
      async connect(options) {
        assert.equal(options.family, "ipv4");
        return { output: socketOutput };
      }
    },
    Interceptor: {
      attach(_target, callbacks) {
        hook = callbacks;
        return {};
      }
    },
    setInterval(callback, delay) {
      intervals.push({ callback, delay });
      return intervals.length;
    },
    clearInterval() {},
    setTimeout(callback, delay) {
      timeouts.push({ callback, delay });
      return timeouts.length;
    },
    clearTimeout() {},
    rpc: { exports: {} }
  });

  vm.runInContext(source, context, { filename: sourceUrl.pathname });
  await flushPromises();
  assert.ok(hook, "Gadget script did not install the native hook");

  function enter(state, bytes = REPORT) {
    const invocation = {};
    const args = Array.from({ length: 10 }, () => u32(0));
    args[4] = new StatusPointer(state);
    args[5] = u32(READ_CHARACTERISTIC_IOCTL);
    args[8] = new OutputPointer(bytes);
    args[9] = u32(bytes.length);
    hook.onEnter.call(invocation, args);
    return invocation;
  }

  function leave(invocation, status) {
    hook.onLeave.call(invocation, u32(status));
  }

  function invoke(state, bytes, status) {
    const invocation = enter(state, bytes);
    leave(invocation, status);
  }

  function runInterval(delay) {
    const interval = intervals.find((candidate) => candidate.delay === delay);
    assert.ok(interval, `No ${delay} ms interval was registered`);
    interval.callback();
  }

  return {
    enter,
    leave,
    invoke,
    runPendingSweep: () => runInterval(10),
    runHeartbeat: () => runInterval(5000),
    intervalDelays: () => intervals.map(({ delay }) => delay),
    messages: () => messages.slice(),
    gattReads: () => messages
      .filter(({ kind }) => kind === "gatt_read")
      .map(({ raw }) => raw)
  };
}
~~~

The harness supplies Process.pointerSize = 8, a successful Socket.connect output,
timer capture, rpc.exports, and an Interceptor.attach callback. Each invocation
builds ten native arguments with the status block at index 4, IOCTL at index 5,
output at index 8, and output length at index 9.

Add tests with these exact assertions:

~~~javascript
test("emits an immediate successful HID report once", async () => {
  const gadget = await loadGadget();
  const state = { id: 1, status: STATUS_SUCCESS, information: 9 };
  gadget.invoke(state, REPORT, STATUS_SUCCESS);
  await flushPromises();
  assert.deepEqual(gadget.gattReads(), ["010000280000000000"]);
});

test("waits for STATUS_PENDING completion and emits it once", async () => {
  const gadget = await loadGadget();
  const state = { id: 2, status: STATUS_PENDING, information: 0 };
  gadget.invoke(state, REPORT, STATUS_PENDING);
  await flushPromises();
  assert.deepEqual(gadget.gattReads(), []);
  state.status = STATUS_SUCCESS;
  state.information = 9;
  gadget.runPendingSweep();
  gadget.runPendingSweep();
  await flushPromises();
  assert.deepEqual(gadget.gattReads(), ["010000280000000000"]);
});

test("does not emit a pending completion more than once", async () => {
  const gadget = await loadGadget();
  const state = { id: 5, status: STATUS_PENDING, information: 0 };
  gadget.invoke(state, REPORT, STATUS_PENDING);
  state.status = STATUS_SUCCESS;
  state.information = 9;
  gadget.runPendingSweep();
  gadget.runPendingSweep();
  await flushPromises();
  assert.deepEqual(gadget.gattReads(), ["010000280000000000"]);
});

test("drops a cancelled asynchronous completion", async () => {
  const gadget = await loadGadget();
  const state = { id: 3, status: STATUS_PENDING, information: 0 };
  gadget.invoke(state, REPORT, STATUS_PENDING);
  state.status = STATUS_CANCELLED;
  gadget.runPendingSweep();
  await flushPromises();
  assert.deepEqual(gadget.gattReads(), []);
});

test("flushes a completed status block before WUDF reuses it", async () => {
  const gadget = await loadGadget();
  const state = { id: 4, status: STATUS_PENDING, information: 0 };
  gadget.invoke(state, REPORT, STATUS_PENDING);
  state.status = STATUS_SUCCESS;
  state.information = 9;
  const second = gadget.enter(state, [1, 0, 0, 0, 0, 0, 0, 0, 0]);
  state.status = STATUS_PENDING;
  state.information = 0;
  gadget.leave(second, STATUS_PENDING);
  await flushPromises();
  assert.deepEqual(gadget.gattReads(), ["010000280000000000"]);
});

test("registers a 10 ms pending completion sweep", async () => {
  const gadget = await loadGadget();
  assert.ok(gadget.intervalDelays().includes(10));
});

test("uses IO_STATUS_BLOCK.Information as the completed byte count", async () => {
  const gadget = await loadGadget();
  const state = { id: 6, status: STATUS_SUCCESS, information: 8 };
  gadget.invoke(state, REPORT, STATUS_SUCCESS);
  await flushPromises();
  assert.deepEqual(gadget.gattReads(), []);
});
~~~

- [ ] **Step 3: Run the focused regression suite and verify RED**

Run: npm run test:gadget

Expected: the immediate-success test passes, while the pending-completion, exactly-once,
status-block-reuse, 10 ms sweep, and completed-length tests fail because version 0.4.1
drops nonzero return statuses and ignores IO_STATUS_BLOCK completion metadata.

- [ ] **Step 4: Commit the failing regression harness**

~~~powershell
git add package.json tests/xiaomi_hid_gadget.test.mjs
git commit -m "test: reproduce pending HID completion loss"
~~~

---

### Task 2: Track asynchronous Gadget completions exactly once

**Files:**
- Modify: src-tauri/src/bridges/xiaomi/xiaomi_hid_gadget.js
- Test: tests/xiaomi_hid_gadget.test.mjs

**Interfaces:**
- Consumes: fake and real NativePointer methods isNull(), add(), readU32(), readU64(), readByteArray(), and toString()
- Produces: sweepPendingIo(), finishIoContext(context, status), pending completion telemetry in heartbeat payloads

- [ ] **Step 1: Add status constants and tracker state**

~~~javascript
const STATUS_SUCCESS = 0x00000000;
const STATUS_PENDING = 0x00000103;
const PENDING_SWEEP_INTERVAL_MS = 10;
const MAX_PENDING_IO = 64;

const pendingIo = new Map();
let nextIoId = 1;
let completedIo = 0;
let failedIo = 0;
~~~

- [ ] **Step 2: Implement completed-length and exactly-once helpers**

~~~javascript
function ioInformation(statusBlock) {
  const information = statusBlock.add(Process.pointerSize);
  return Process.pointerSize === 8
    ? information.readU64().toNumber()
    : information.readU32();
}

function removePending(context) {
  if (pendingIo.get(context.key) === context) pendingIo.delete(context.key);
}

function finishIoContext(context, status) {
  if (status === STATUS_PENDING) return false;
  removePending(context);
  if (status !== STATUS_SUCCESS) {
    failedIo += 1;
    return true;
  }
  const actualLength = ioInformation(context.statusBlock);
  completedIo += 1;
  if (
    !context.output.isNull() &&
    context.outputLength >= EXPECTED_OUTPUT_LENGTH &&
    actualLength === EXPECTED_OUTPUT_LENGTH
  ) {
    emit({ kind: "gatt_read", raw: hex(context.output, actualLength) });
  }
  return true;
}
~~~

- [ ] **Step 3: Implement bounded pending registration and sweeping**

~~~javascript
function sweepPendingIo() {
  for (const context of Array.from(pendingIo.values())) {
    try {
      finishIoContext(context, context.statusBlock.readU32());
    } catch (_error) {
      removePending(context);
      failedIo += 1;
    }
  }
}

function rememberPending(context) {
  while (pendingIo.size >= MAX_PENDING_IO) {
    const oldestKey = pendingIo.keys().next().value;
    pendingIo.delete(oldestKey);
    failedIo += 1;
  }
  pendingIo.set(context.key, context);
}
~~~

- [ ] **Step 4: Replace the immediate-return-only hook**

On matching IOCTL entry, sweep previous completions and retain args[4], args[8],
and args[9]. On return, remember STATUS_PENDING or finish a terminal result:

~~~javascript
onEnter(args) {
  this.capture = args[5].toUInt32() === READ_CHARACTERISTIC_IOCTL;
  if (!this.capture) return;
  sweepPendingIo();
  this.ioContext = {
    id: nextIoId++,
    key: args[4].toString(),
    statusBlock: args[4],
    output: args[8],
    outputLength: args[9].toUInt32()
  };
},
onLeave(retval) {
  if (!this.capture) return;
  const status = retval.toUInt32();
  try {
    if (status === STATUS_PENDING) {
      if (!this.ioContext.statusBlock.isNull() && !this.ioContext.output.isNull()) {
        rememberPending(this.ioContext);
      }
      return;
    }
    finishIoContext(this.ioContext, status);
  } catch (_error) {
    removePending(this.ioContext);
    failedIo += 1;
  }
}
~~~

Start setInterval(sweepPendingIo, PENDING_SWEEP_INTERVAL_MS). Extend the existing
heartbeat object with pending_io, completed_io, and failed_io fields.

- [ ] **Step 5: Run the focused suite and verify GREEN**

Run: npm run test:gadget

Expected: all immediate, pending, cancellation, reuse, and timer tests pass.

- [ ] **Step 6: Run the existing frontend suite**

Run: npx vitest run

Expected: all existing Vitest tests pass with zero failures.

- [ ] **Step 7: Commit the Gadget fix**

~~~powershell
git add src-tauri/src/bridges/xiaomi/xiaomi_hid_gadget.js tests/xiaomi_hid_gadget.test.mjs
git commit -m "fix: capture pending HID completions"
~~~

---

### Task 3: Serialize full HID Tap restarts and enable future script reloads

**Files:**
- Modify: src-tauri/src/ipc/commands.rs
- Modify: src-tauri/src/bridges/xiaomi/hid_tap_runtime.rs
- Test: Rust unit tests in both files

**Interfaces:**
- Consumes: hid_report_tap::stop_and_join(), XIAOMI_LIFECYCLE_LOCK, existing BLE runtime stop/restart logic
- Produces: run_restart_teardown(stop_hid_tap, stop_worker) and Gadget config with on_change = reload

- [ ] **Step 1: Add failing restart-order and config tests**

In commands.rs:

~~~rust
#[test]
fn restart_teardown_stops_hid_tap_before_worker() {
    use std::cell::RefCell;
    let order = RefCell::new(Vec::new());
    run_restart_teardown(
        || order.borrow_mut().push("hid_tap"),
        || order.borrow_mut().push("worker"),
    );
    assert_eq!(*order.borrow(), ["hid_tap", "worker"]);
}
~~~

In hid_tap_runtime.rs:

~~~rust
#[test]
fn gadget_config_reloads_script_changes() {
    let config = super::gadget_config_text();
    assert!(config.contains(r#""on_change": "reload""#));
    assert!(!config.contains(r#""on_change": "ignore""#));
}
~~~

- [ ] **Step 2: Run the focused Rust tests and verify RED**

Run: cargo test --manifest-path src-tauri/Cargo.toml restart_teardown_stops_hid_tap_before_worker

Run: cargo test --manifest-path src-tauri/Cargo.toml gadget_config_reloads_script_changes

Expected: compilation/test failure because run_restart_teardown does not exist and the config still contains ignore.

- [ ] **Step 3: Add the teardown-order helper and use it under the lock**

~~~rust
fn run_restart_teardown(
    stop_hid_tap: impl FnOnce(),
    stop_worker: impl FnOnce(),
) {
    stop_hid_tap();
    stop_worker();
}
~~~

Immediately after acquiring XIAOMI_LIFECYCLE_LOCK, call the helper with
hid_report_tap::stop_and_join as the first closure and the existing runtime
request_stop/cancel/reset/wait block as the second closure.

Remove the standalone stop_and_join call from run_atvv_repair_pipeline so concurrent
repair clicks cannot stop the hub outside the lifecycle lock.

- [ ] **Step 4: Enable Frida script reload for future versions**

Change the generated config field exactly once:

~~~rust
"on_change": "reload"
~~~

- [ ] **Step 5: Run focused and full Rust tests**

Run: cargo test --manifest-path src-tauri/Cargo.toml restart_teardown_stops_hid_tap_before_worker

Run: cargo test --manifest-path src-tauri/Cargo.toml gadget_config_reloads_script_changes

Run: cargo test --manifest-path src-tauri/Cargo.toml --workspace

Expected: every command exits zero.

- [ ] **Step 6: Commit lifecycle changes**

~~~powershell
git add src-tauri/src/ipc/commands.rs src-tauri/src/bridges/xiaomi/hid_tap_runtime.rs
git commit -m "fix: rebuild HID tap on bridge restart"
~~~

---

### Task 4: Version 0.4.2 and document the one-time host reload

**Files:**
- Modify: package.json
- Modify: package-lock.json
- Modify: src-tauri/Cargo.toml
- Modify: src-tauri/Cargo.lock
- Modify: src-tauri/tauri.conf.json
- Modify: CHANGELOG.md

**Interfaces:**
- Consumes: completed behavior changes from Tasks 2 and 3
- Produces: consistent application/package version 0.4.2

- [ ] **Step 1: Add a failing metadata consistency check**

Run before editing:

~~~powershell
$versions = @(
  (Get-Content package.json -Raw | ConvertFrom-Json).version
  (Get-Content src-tauri/tauri.conf.json -Raw | ConvertFrom-Json).version
  ((Select-String -Path src-tauri/Cargo.toml -Pattern '^version = "([^"]+)"$' | Select-Object -First 1).Matches[0].Groups[1].Value)
)
if (($versions | Select-Object -Unique).Count -ne 1 -or $versions[0] -ne '0.4.2') {
  throw "expected every application version to be 0.4.2; got $($versions -join ', ')"
}
~~~

Expected: failure showing 0.4.1.

- [ ] **Step 2: Update Node and Tauri metadata**

Run: npm version 0.4.2 --no-git-tag-version

Edit src-tauri/Cargo.toml and src-tauri/tauri.conf.json to 0.4.2. Run cargo
metadata once so src-tauri/Cargo.lock records the root package version.

- [ ] **Step 3: Add the changelog entry**

Add a 2026-09-10 section describing asynchronous HID completion capture, serialized
HID Tap restart, preserved mappings, and the required one-time Bluetooth off/on cycle
after upgrading from 0.4.1.

- [ ] **Step 4: Re-run the metadata consistency check**

Expected: all three primary metadata files report exactly 0.4.2; package-lock.json
and Cargo.lock contain the same root package version.

- [ ] **Step 5: Commit version metadata**

~~~powershell
git add package.json package-lock.json src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/tauri.conf.json CHANGELOG.md
git commit -m "chore: prepare Nexus Prime 0.4.2"
~~~

---

### Task 5: Full verification and NSIS packaging

**Files:**
- Verify only; do not modify production files unless a failing check identifies a defect
- Output: src-tauri/target/release/bundle/nsis/Nexus Prime_0.4.2_x64-setup.exe

**Interfaces:**
- Consumes: all earlier task outputs
- Produces: verified installer plus exact hashes and file size

- [ ] **Step 1: Verify clean formatting and diff**

Run: git diff --check

Run: cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check

- [ ] **Step 2: Run all automated tests**

Run: npm test

Run: cargo test --manifest-path src-tauri/Cargo.toml --workspace

- [ ] **Step 3: Run compile checks**

Run: npm run build

Run: cargo check --manifest-path src-tauri/Cargo.toml --workspace --all-targets

- [ ] **Step 4: Build the installer**

Run: npm run tauri:build

Expected: Tauri exits zero and creates the 0.4.2 NSIS current-user installer.

- [ ] **Step 5: Record artifact integrity**

~~~powershell
$installer = Get-ChildItem src-tauri/target/release/bundle/nsis/*0.4.2*x64-setup.exe |
  Select-Object -First 1
if (-not $installer) { throw '0.4.2 NSIS installer not found' }
Get-Item $installer.FullName | Select-Object FullName, Length, LastWriteTime
Get-FileHash -Algorithm SHA256 $installer.FullName
~~~

- [ ] **Step 6: Commit any verification-only documentation update**

Do not commit generated target or dist files. Ensure git status contains no accidental
build artifacts before reporting the installer.

---

### Task 6: Real-device acceptance after installation

**Files:**
- Runtime verification only
- Inspect: APPDATA Nexus Prime daily log

**Interfaces:**
- Consumes: installed 0.4.2 NSIS package and paired RC003
- Produces: evidence that asynchronous reports remain device-correlated under repeated use

- [ ] **Step 1: Install 0.4.2 over 0.4.1**

Exit Nexus Prime from the tray, run the generated installer, and preserve the existing
installation directory and APPDATA configuration.

- [ ] **Step 2: Unload the old 0.4.1 Gadget once**

Turn Windows Bluetooth off and back on, or restart Windows. Reopen Nexus Prime and
accept the HID Tap UAC request.

- [ ] **Step 3: Exercise the original failure pattern**

Press at least 30 events including repeated OK, alternating directions, Back, volume,
and voice. Include idle gaps longer than 30 seconds before additional presses.

- [ ] **Step 4: Verify logs**

Expected:

- HID TAP key entries continue after the previous 2-10 event failure point.
- No later native VK observe entry appears without a nearby device-correlated HID Tap report for the same remote press.
- Restart key bridge records stop_and_join requested/done followed by a new hub start.
- No duplicate mapped action appears for one physical press.

- [ ] **Step 5: Mark hardware acceptance**

Record the exact test time range and result in progress.md. If hardware acceptance
fails, retain the installer and logs, return to root-cause investigation, and do not
claim the bug fixed.
