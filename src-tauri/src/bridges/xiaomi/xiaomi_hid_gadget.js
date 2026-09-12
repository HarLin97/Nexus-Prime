const READ_CHARACTERISTIC_IOCTL = 0x80018483;
const EXPECTED_OUTPUT_LENGTH = 9;
const STATUS_SUCCESS = 0x00000000;
const STATUS_PENDING = 0x00000103;
const PENDING_SWEEP_INTERVAL_MS = 10;
const MAX_PENDING_IO = 64;
const HEARTBEAT_INTERVAL_MS = 5000;
const RECONNECT_DELAY_MS = 1000;

let host = "127.0.0.1";
let port = 30684;
let connection = null;
let output = null;
let writeChain = Promise.resolve();
let reconnectTimer = null;
let hookInstalled = false;
const pendingIo = new Map();
let nextIoId = 1;
let completedIo = 0;
let failedIo = 0;

function asciiBytes(text) {
  const result = [];
  for (let index = 0; index < text.length; index++) {
    result.push(text.charCodeAt(index) & 0xff);
  }
  return result;
}

function hex(pointer, length) {
  if (pointer.isNull() || length <= 0) return "";
  const bytes = new Uint8Array(pointer.readByteArray(length));
  let result = "";
  for (let index = 0; index < bytes.length; index++) {
    result += bytes[index].toString(16).padStart(2, "0");
  }
  return result;
}

function scheduleReconnect() {
  if (reconnectTimer !== null) return;
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    connectToHub();
  }, RECONNECT_DELAY_MS);
}

function markDisconnected(currentOutput) {
  if (output !== currentOutput) return;
  output = null;
  connection = null;
  scheduleReconnect();
}

function emit(payload) {
  const currentOutput = output;
  if (currentOutput === null) {
    scheduleReconnect();
    return;
  }
  const line = JSON.stringify(payload) + "\n";
  writeChain = writeChain
    .then(() => currentOutput.writeAll(asciiBytes(line)))
    .catch(() => markDisconnected(currentOutput));
}

function ioInformation(statusBlock) {
  const information = statusBlock.add(Process.pointerSize);
  return Process.pointerSize === 8
    ? information.readU64().toNumber()
    : information.readU32();
}

function removePending(context) {
  if (pendingIo.get(context.key) === context) {
    pendingIo.delete(context.key);
  }
}

function finishIoContext(context, status, immediate = false) {
  if (status === STATUS_PENDING) return false;
  removePending(context);
  if (status !== STATUS_SUCCESS) {
    failedIo += 1;
    return true;
  }

  // On this RC003/WUDF path an immediately successful call historically
  // delivered the nine-byte report reliably even when IO_STATUS_BLOCK.Information
  // was not yet the expected value at our return hook. v0.4.2 made Information
  // mandatory for every completion and live logs then stopped reaching HID TAP
  // READY altogether. Preserve the proven immediate-success contract (the caller
  // requested exactly one nine-byte report); pending completions still require
  // the kernel-reported completed byte count before their buffer is trusted.
  const actualLength = immediate ? context.outputLength : ioInformation(context.statusBlock);
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

async function connectToHub() {
  if (output !== null) return;
  try {
    const currentConnection = await Socket.connect({
      family: "ipv4",
      host: host,
      port: port
    });
    connection = currentConnection;
    output = currentConnection.output;
    emit({ kind: "ready", pid: Process.id, hook_installed: hookInstalled });
  } catch (_error) {
    connection = null;
    output = null;
    scheduleReconnect();
  }
}

function installHook() {
  if (hookInstalled) return;
  const ntdll = Process.findModuleByName("ntdll.dll");
  const target = ntdll ? ntdll.findExportByName("NtDeviceIoControlFile") : null;
  if (target === null) {
    emit({ kind: "error", message: "NtDeviceIoControlFile export not found" });
    return;
  }
  Interceptor.attach(target, {
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
          if (
            !this.ioContext.statusBlock.isNull() &&
            !this.ioContext.output.isNull()
          ) {
            rememberPending(this.ioContext);
          }
          return;
        }
        finishIoContext(this.ioContext, status, true);
      } catch (_error) {
        removePending(this.ioContext);
        failedIo += 1;
      }
    }
  });
  hookInstalled = true;
}

setInterval(sweepPendingIo, PENDING_SWEEP_INTERVAL_MS);

setInterval(() => {
  if (output === null) {
    scheduleReconnect();
  } else {
    emit({
      kind: "heartbeat",
      pid: Process.id,
      pending_io: pendingIo.size,
      completed_io: completedIo,
      failed_io: failedIo
    });
  }
}, HEARTBEAT_INTERVAL_MS);

rpc.exports = {
  async init(_stage, parameters) {
    host = (parameters && parameters.host) || host;
    port = (parameters && parameters.port) || port;
    installHook();
    await connectToHub();
  }
};

// LoadLibrary 注入后部分 Gadget 版本不会立刻调 init：脚本加载时主动挂钩并连 hub
try {
  installHook();
  connectToHub();
} catch (_error) {
  // init 路径仍会重试
}
