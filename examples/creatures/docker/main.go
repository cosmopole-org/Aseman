package main

import (
	"bufio"
	"encoding/binary"
	"encoding/json"
	"io"
	"log"
	"net"
	"os"
	"sync"
)

// Docker sample mirrors machines/docker callback channel style and exposes
// helper wrappers for host APIs routed by appengine + vmm.

var lock sync.Mutex
var conn net.Conn
var callbackCounter int64

func writePacket(data []byte, withCallback bool) {
	lock.Lock()
	defer lock.Unlock()
	lenBytes := make([]byte, 4)
	binary.LittleEndian.PutUint32(lenBytes, uint32(len(data)))
	_, _ = conn.Write(lenBytes)
	cb := make([]byte, 8)
	if withCallback {
		callbackCounter++
		binary.LittleEndian.PutUint64(cb, uint64(callbackCounter))
	}
	_, _ = conn.Write(cb)
	_, _ = conn.Write(data)
}

func hostCall(key string, input map[string]any) {
	packet, _ := json.Marshal(map[string]any{"key": key, "input": input})
	writePacket(packet, true)
}

func SignalStore(typ string, storeID string, userID string, data any) {
	body, _ := json.Marshal(data)
	hostCall("signalStore", map[string]any{"type": typ, "storeId": storeID, "userId": userID, "data": string(body)})
}

func HttpPost(url string, headers string, body string) {
	hostCall("httpPost", map[string]any{"url": url, "headers": headers, "body": body})
}

func RunVm(machineID string, input string, astPath string, runtime string) {
	hostCall("runVm", map[string]any{"machineId": machineID, "input": input, "astPath": astPath, "runtime": runtime})
}

func ExecVm(machineID string, imageName string, containerName string, command string) {
	hostCall("execVm", map[string]any{"machineId": machineID, "imageName": imageName, "containerName": containerName, "command": command})
}

func CopyToVm(machineID string, imageName string, containerName string, fileName string, content string) {
	hostCall("copyToVm", map[string]any{"machineId": machineID, "imageName": imageName, "containerName": containerName, "fileName": fileName, "content": content})
}

func CheckTokenValidity(token string) { hostCall("checkTokenValidity", map[string]any{"token": token}) }
func TerminateVm(machineID string)    { hostCall("terminateVm", map[string]any{"machineId": machineID}) }

func PlantTrigger(tag string, input string, storeID string, count int) {
	hostCall("plantTrigger", map[string]any{"tag": tag, "input": input, "storeId": storeID, "count": count})
}

func SendMessageOnChain(storeID string, payload string) {
	hostCall("sendMessageOnChain", map[string]any{"storeId": storeID, "payload": payload})
}

func RunDocker(machineID string, storeID string, containerMeta string) {
	hostCall("runVm", map[string]any{"machineId": machineID, "storeId": storeID, "containerMeta": containerMeta, "runtime": "docker"})
}

func ExecDocker(machineID string, imageName string, containerName string, command string) {
	ExecVm(machineID, imageName, containerName, command)
}

func CopyToDocker(machineID string, imageName string, containerName string, fileName string, content string) {
	CopyToVm(machineID, imageName, containerName, fileName, content)
}

func Log(text string) { hostCall("log", map[string]any{"text": text}) }

func processPacket(data []byte) {
	packet := map[string]any{}
	if err := json.Unmarshal(data, &packet); err != nil {
		log.Println(err)
		return
	}
	payload := map[string]any{}
	if err := json.Unmarshal([]byte(packet["data"].(string)), &payload); err != nil {
		log.Println(err)
		return
	}
	if payload["type"] == "textMessage" {
		SignalStore("broadcast", packet["store"].(map[string]any)["id"].(string), packet["user"].(map[string]any)["id"].(string), map[string]any{
			"type": "textMessage",
			"text": "docker sdk echo: " + payload["text"].(string),
		})
	}
}

func main() {
	var err error
	conn, err = net.Dial("tcp", "10.10.0.3:8084")
	if err != nil {
		log.Fatalf("dial error: %v", err)
	}
	defer conn.Close()

	reader := bufio.NewReader(conn)
	for {
		var ln uint32
		if err := binary.Read(reader, binary.LittleEndian, &ln); err != nil {
			if err != io.EOF {
				log.Printf("read len err: %v", err)
			}
			os.Exit(0)
		}
		var callbackID uint64
		if err := binary.Read(reader, binary.LittleEndian, &callbackID); err != nil {
			if err != io.EOF {
				log.Printf("read callback err: %v", err)
			}
			os.Exit(0)
		}
		body := make([]byte, ln)
		if _, err := io.ReadFull(reader, body); err != nil {
			log.Printf("read body err: %v", err)
			os.Exit(0)
		}
		if callbackID == 0 {
			processPacket(body)
		}
	}
}
