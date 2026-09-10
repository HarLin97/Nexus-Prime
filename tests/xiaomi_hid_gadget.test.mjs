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
  constructor(bytes) {
    this.bytes = Uint8Array.from(bytes);
  }

  isNull() {
    return false;
  }

  readByteArray(length) {
    return this.bytes.slice(0, length).buffer;
  }

  toString() {
    return "[output]";
  }
}

class StatusPointer {
  constructor(state, offset = 0) {
    this.state = state;
    this.offset = offset;
  }

  isNull() {
    return false;
  }

  add(offset) {
    return new StatusPointer(this.state, this.offset + offset);
  }

  readU32() {
    assert.equal(this.offset, 0);
    return this.state.status >>> 0;
  }

  readU64() {
    assert.equal(this.offset, 8);
    return { toNumber: () => this.state.information };
  }

  toString() {
    return `iosb:${this.state.id}`;
  }
}

async function flushPromises() {
  for (let index = 0; index < 8; index += 1) {
    await Promise.resolve();
  }
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
      const payload = String.fromCharCode(...bytes);
      for (const line of payload.split("\n")) {
        if (line) {
          messages.push(JSON.parse(line));
        }
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
      .map(({ raw }) => raw),
    heartbeats: () => messages.filter(({ kind }) => kind === "heartbeat")
  };
}

test("emits an immediate successful HID report once", async () => {
  const gadget = await loadGadget();
  const state = { id: 1, status: STATUS_SUCCESS, information: 9 };
  gadget.invoke(state, REPORT, STATUS_SUCCESS);
  await flushPromises();
  assert.deepEqual(gadget.gattReads(), ["010000280000000000"]);
});

test("waits for STATUS_PENDING completion and then emits", async () => {
  const gadget = await loadGadget();
  const state = { id: 2, status: STATUS_PENDING, information: 0 };
  gadget.invoke(state, REPORT, STATUS_PENDING);
  await flushPromises();
  assert.deepEqual(gadget.gattReads(), []);

  state.status = STATUS_SUCCESS;
  state.information = 9;
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

test("does not emit a pending completion more than once", async () => {
  const gadget = await loadGadget();
  const state = { id: 4, status: STATUS_PENDING, information: 0 };
  gadget.invoke(state, REPORT, STATUS_PENDING);
  state.status = STATUS_SUCCESS;
  state.information = 9;
  gadget.runPendingSweep();
  gadget.runPendingSweep();
  await flushPromises();
  assert.deepEqual(gadget.gattReads(), ["010000280000000000"]);
});

test("flushes a completed status block before WUDF reuses it", async () => {
  const gadget = await loadGadget();
  const state = { id: 5, status: STATUS_PENDING, information: 0 };
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

test("bounds retained pending I/O contexts", async () => {
  const gadget = await loadGadget();
  const states = Array.from({ length: 65 }, (_, index) => ({
    id: 100 + index,
    status: STATUS_PENDING,
    information: 0
  }));

  for (const state of states) {
    gadget.invoke(state, REPORT, STATUS_PENDING);
  }

  states[0].status = STATUS_SUCCESS;
  states[0].information = 9;
  gadget.runPendingSweep();
  await flushPromises();
  assert.deepEqual(gadget.gattReads(), []);

  states.at(-1).status = STATUS_SUCCESS;
  states.at(-1).information = 9;
  gadget.runPendingSweep();
  await flushPromises();
  assert.deepEqual(gadget.gattReads(), ["010000280000000000"]);
});

test("reports pending completion telemetry in heartbeats", async () => {
  const gadget = await loadGadget();

  gadget.invoke(
    { id: 200, status: STATUS_SUCCESS, information: 9 },
    REPORT,
    STATUS_SUCCESS
  );
  gadget.invoke(
    { id: 201, status: STATUS_CANCELLED, information: 0 },
    REPORT,
    STATUS_CANCELLED
  );
  gadget.invoke(
    { id: 202, status: STATUS_PENDING, information: 0 },
    REPORT,
    STATUS_PENDING
  );
  gadget.runHeartbeat();
  await flushPromises();

  const heartbeat = gadget.heartbeats().at(-1);
  assert.equal(heartbeat.pending_io, 1);
  assert.equal(heartbeat.completed_io, 1);
  assert.equal(heartbeat.failed_io, 1);
});
