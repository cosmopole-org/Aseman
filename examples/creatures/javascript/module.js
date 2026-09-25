// QuickJS-compatible server-side JavaScript sample for Kasper appengine.
// Expects a global hostCall(reqJsonString) -> responseJsonString bridge.

function host(op, input) {
  return JSON.parse(hostCall(JSON.stringify({ op, input })) || "{}");
}

const vm = {
  httpPost: (url, headers, body) => host("httpPost", { url, headers, body }),
  plantTrigger: (tag, input, storeId, count) => host("plantTrigger", { tag, input, storeId, count }),
  signalStore: (type, storeId, userId, data) => host("signalStore", { type, storeId, userId, data: JSON.stringify(data) }),
  runVm: (machineId, input, astPath, vmType) => host("runVm", { machineId, input, astPath, vmType }),
  execVm: (machineId, imageName, containerName, command) => host("execVm", { machineId, imageName, containerName, command }),
  copyToVm: (machineId, imageName, containerName, fileName, content) => host("copyToVm", { machineId, imageName, containerName, fileName, content }),
  checkTokenValidity: (token) => host("checkTokenValidity", { token }),
  sendMessageOnChain: (storeId, payload) => host("sendMessageOnChain", { storeId, payload }),
  terminateVm: (machineId) => host("terminateVm", { machineId }),
  submitOnchainTrx: (targetMachineId, key, packet, meta) => host("submitOnchainTrx", { targetMachineId, key, packet, meta }),
  dbPut: (key, val) => host("dbOp", { op: "put", key, val }),
  dbGet: (key) => host("dbOp", { op: "get", key }),
  dbDel: (key) => host("dbOp", { op: "del", key }),
  dbGetByPrefix: (prefix) => host("dbOp", { op: "getByPrefix", prefix }),
  log: (text) => host("consoleLog", { text }),
  output: (text) => host("output", { text }),
  lockResource: (resourceId, ownerId) => host("lockResource", { runtime: "javascript", resourceId, ownerId }),
  unlockResource: (resourceId, ownerId) => host("unlockResource", { runtime: "javascript", resourceId, ownerId }),
  newSyncTask: (name, deps) => host("newSyncTask", { name, deps })
};

function main(inputRaw) {
  const payload = typeof inputRaw === "string" ? JSON.parse(inputRaw || "{}") : (inputRaw || {});
  const sku = payload.sku || "unknown";
  const qty = Number(payload.qty || 0);
  const key = `stock::${sku}`;

  vm.lockResource(`inventory::${sku}`, "sdk-js-sample");
  const prev = vm.dbGet(key);
  vm.log(`previous stock for ${sku}: ${JSON.stringify(prev)}`);
  vm.dbPut(key, String(qty));
  vm.output(JSON.stringify({ ok: true, sku, qty }));
  vm.unlockResource(`inventory::${sku}`, "sdk-js-sample");

  return { ok: true, sku, qty };
}

main('{"sku":"SKU-100","qty":42}');
