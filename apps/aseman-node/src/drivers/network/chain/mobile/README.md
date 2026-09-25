# Mobile Chain Build Guide 📱

> Updated: **2026-04-10**

## Build from Source

### Dependencies

- Java JDK
- Android NDK
- Go mobile tools

```bash
go install golang.org/x/mobile/cmd/gomobile@latest
gomobile init -ndk ~/PATH/TO/ANDROID/NDK
```

### Build Android Library

```bash
gomobile bind -v -target=android kasper/src/drivers/network/chain/babble/mobile
```

## Import the Babble Module

Reference workflow:

- <https://stackoverflow.com/questions/16682847/how-to-manually-include-external-aar-package-using-new-gradle-android-build-syst>

If Android Studio shows “cannot resolve symbol” while build still compiles:

- `File -> Invalidate Caches... -> Invalidate and Restart`
