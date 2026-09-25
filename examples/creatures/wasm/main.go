package main

import (
	"aseman.example/creatures/wasm/sdk"
	"strings"
	"unsafe"
)

var heap = make([]byte, 1024*1024)
var heapPtr uint32 = 8

//export malloc
func malloc(size uint32) uint32 {
	ptr := heapPtr
	heapPtr += size
	return ptr
}

func readInput(arg uint64) string {
	offset := uint32(arg >> 32)
	length := uint32(arg)
	return string(unsafe.Slice((*byte)(unsafe.Pointer(uintptr(offset))), length))
}

// Real-world server logic sample: maintain per-SKU reserve stock counters.
//
//export run
func run(arg uint64) int64 {
	input := readInput(arg)
	sku := "unknown"
	qty := 0

	for _, chunk := range strings.Split(strings.Trim(input, "{}"), ",") {
		kv := strings.SplitN(chunk, ":", 2)
		if len(kv) != 2 {
			continue
		}
		k := strings.Trim(kv[0], " \"")
		v := strings.Trim(kv[1], " \"")
		if k == "sku" {
			sku = v
		}
		if k == "qty" {
			for _, c := range v {
				if c >= '0' && c <= '9' {
					qty = qty*10 + int(c-'0')
				}
			}
		}
	}

	lockKey := "inventory::" + sku
	sdk.LockResource(lockKey, "sdk-wasm-sample")
	current := sdk.Get("stock::" + sku)
	sdk.ConsoleLog("stock before update " + current)
	sdk.Put("stock::"+sku, sdk.Itoa(qty))
	sdk.Output("updated stock for " + sku)
	sdk.UnlockResource(lockKey, "sdk-wasm-sample")
	return 0
}

func main() {}
