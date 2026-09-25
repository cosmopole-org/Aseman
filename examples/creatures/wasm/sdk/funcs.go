package sdk

import (
	"encoding/json"
	"fmt"
	"unsafe"
)

// AppEngine-compatible single host import.
//
//go:wasmimport env hostCall
func hostCall(offset uint32, length uint32) uint64

type hostEnvelope struct {
	Op    string      `json:"op"`
	Key   string      `json:"key"`
	Input interface{} `json:"input"`
}

func bytesAt(offset uint32, length uint32) []byte {
	return unsafe.Slice((*byte)(unsafe.Pointer(uintptr(offset))), length)
}

func stringAt(offset uint32, length uint32) string {
	return string(bytesAt(offset, length))
}

func hostRequestBytes(req []byte) string {
	if len(req) == 0 {
		return ""
	}
	ptr := uint32(uintptr(unsafe.Pointer(&req[0])))
	ret := hostCall(ptr, uint32(len(req)))
	retOffset := uint32(ret >> 32)
	retLen := uint32(ret)
	return stringAt(retOffset, retLen)
}

func callWithKey(op string, key string, input interface{}) string {
	env := hostEnvelope{Op: op, Key: key, Input: input}
	payload, err := json.Marshal(env)
	if err != nil {
		return `{"ok":false,"error":"sdk marshal failed"}`
	}
	return hostRequestBytes(payload)
}

func call(op string, input interface{}) string {
	return callWithKey(op, op, input)
}

func callRaw(op string, input string) string {
	var raw json.RawMessage = []byte(input)
	if !json.Valid(raw) {
		raw = json.RawMessage(`{}`)
	}
	env := hostEnvelope{Op: op, Key: op, Input: raw}
	payload, err := json.Marshal(env)
	if err != nil {
		return `{"ok":false,"error":"sdk marshal failed"}`
	}
	return hostRequestBytes(payload)
}

// ---- Full helper surface mirroring machines/wasm sdk host capabilities ----

func HttpPost(url string, headers string, body string) string {
	return call("httpPost", map[string]interface{}{"url": url, "headers": headers, "body": body})
}

func PlantTrigger(tag string, input string, storeID string, count int) string {
	return call("plantTrigger", map[string]interface{}{"tag": tag, "input": input, "storeId": storeID, "count": count})
}

func SignalStore(typ string, storeID string, userID string, data string) string {
	return call("signal", map[string]interface{}{"type": typ, "storeId": storeID, "userId": userID, "data": data})
}

func RunDocker(machineID string, storeID string, containerMeta string) string {
	return call("runVm", map[string]interface{}{"machineId": machineID, "storeId": storeID, "containerMeta": containerMeta, "runtime": "docker"})
}

func ExecDocker(machineID string, imageName string, containerName string, command string) string {
	return call("execVm", map[string]interface{}{"machineId": machineID, "imageName": imageName, "containerName": containerName, "command": command})
}

func CopyToDocker(machineID string, imageName string, containerName string, fileName string, content string) string {
	return call("copyToVm", map[string]interface{}{"machineId": machineID, "imageName": imageName, "containerName": containerName, "fileName": fileName, "content": content})
}

func Put(key string, val string) string {
	return call("dbOp", map[string]interface{}{"op": "put", "key": key, "val": val})
}

func Del(key string) string {
	return call("dbOp", map[string]interface{}{"op": "del", "key": key})
}

func Get(key string) string {
	return call("dbOp", map[string]interface{}{"op": "get", "key": key})
}

func GetByPrefix(prefix string) string {
	return call("dbOp", map[string]interface{}{"op": "getByPrefix", "prefix": prefix})
}

func ConsoleLog(text string) string {
	return call("consoleLog", map[string]interface{}{"text": text})
}

func SubmitOnchainTrx(targetMachineID string, key string, packet string, tag string, isFile bool, isBase bool) string {
	meta := "0"
	if isBase {
		meta = "1"
	}
	if isFile {
		meta += "1"
	} else {
		meta += "0"
	}
	meta += tag
	return call("submitOnchainTrx", map[string]interface{}{"targetMachineId": targetMachineID, "key": key, "packet": packet, "meta": meta})
}

func Output(text string) string {
	return call("output", map[string]interface{}{"text": text})
}

func NewSyncTask(name string, deps []string) string {
	return call("newSyncTask", map[string]interface{}{"name": name, "deps": deps})
}

func CheckTokenValidity(token string) string {
	return call("checkTokenValidity", map[string]interface{}{"token": token})
}

func SendMessageOnChain(storeID string, payload string) string {
	return call("sendMessageOnChain", map[string]interface{}{"storeId": storeID, "payload": payload})
}

func RunVm(machineID string, input string, astPath string, runtime string) string {
	return call("runVm", map[string]interface{}{"machineId": machineID, "input": input, "astPath": astPath, "runtime": runtime})
}

func TerminateVm(machineID string) string {
	return call("terminateVm", map[string]interface{}{"machineId": machineID})
}

func LockResource(resourceID string, ownerID string) string {
	return call("lockResource", map[string]interface{}{"runtime": "wasm", "resourceId": resourceID, "ownerId": ownerID})
}

func UnlockResource(resourceID string, ownerID string) string {
	return call("unlockResource", map[string]interface{}{"runtime": "wasm", "resourceId": resourceID, "ownerId": ownerID})
}

func Signal(input string) string        { return callRaw("signal", input) }
func CreateStore(input string) string   { return callRaw("createStore", input) }
func DeleteStore(input string) string   { return callRaw("deleteStore", input) }
func CreateAccess(input string) string  { return callRaw("createAccess", input) }
func DeleteAccess(input string) string  { return callRaw("deleteAccess", input) }
func Transfer(input string) string      { return callRaw("transfer", input) }
func ConsumeLock(input string) string   { return callRaw("consumeLock", input) }
func LockToken(input string) string     { return callRaw("lockToken", input) }
func CreateProgram(input string) string { return callRaw("createProgram", input) }
func DeleteProgram(input string) string { return callRaw("deleteProgram", input) }
func DeployEntity(input string) string  { return callRaw("deployEntity", input) }

func CreateCreature(input string) string { return callRaw("createCreature", input) }
func UpdateCreature(input string) string { return callRaw("updateCreature", input) }
func DeleteCreature(input string) string { return callRaw("deleteCreature", input) }
func GetCreature(input string) string    { return callRaw("getCreature", input) }
func ListCreatures(input string) string  { return callRaw("listCreatures", input) }

func CreateResourceStore(input string) string { return callRaw("createStore", input) }
func UpdateResourceStore(input string) string { return callRaw("createStore", input) }
func DeleteResourceStore(input string) string { return callRaw("deleteStore", input) }
func GetResourceStore(input string) string    { return callRaw("getResourceStore", input) }
func ListResourceStores(input string) string  { return callRaw("listResourceStores", input) }

func CreateResourceEntity(input string) string { return callRaw("deployEntity", input) }
func DeleteResourceEntity(input string) string { return callRaw("deleteStore", input) }

func CreateWorkchain(input string) string { return callRaw("createWorkchain", input) }
func DeleteWorkchain(input string) string { return callRaw("deleteWorkchain", input) }
func CreateSubchain(input string) string  { return callRaw("createSubchain", input) }
func DeleteSubchain(input string) string  { return callRaw("deleteSubchain", input) }

func ExecShellAction(input string) string { return callRaw("execShellAction", input) }

func GenId(input string) string            { return callRaw("genId", input) }
func GetLink(input string) string          { return callRaw("getLink", input) }
func PutLink(input string) string          { return callRaw("putLink", input) }
func DelKey(input string) string           { return callRaw("delKey", input) }
func GetJson(input string) string          { return callRaw("getJson", input) }
func PutJson(input string) string          { return callRaw("putJson", input) }
func GetByPrefixRaw(input string) string   { return callRaw("getByPrefix", input) }
func HasAccessToStore(input string) string { return callRaw("hasAccessToStore", input) }
func SignalUser(input string) string       { return callRaw("signalUser", input) }
func SignalGroup(input string) string      { return callRaw("signalGroup", input) }
func JoinGroup(input string) string        { return callRaw("joinGroup", input) }

func Itoa(v int) string { return fmt.Sprintf("%d", v) }
