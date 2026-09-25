# Mobile Release Procedure 📦

> Updated: **2026-04-10**

## 1) Version Alignment Checks

Mobile releases for `babble` and `babble-android` should use aligned Android SDK versions.

In `babble`, `/docker/mobile/Dockerfile` defines Android SDK and Go versions, for example:

```dockerfile
ENV SDK_URL="https://dl.google.com/android/repository/sdk-tools-linux-4333796.zip" \
    ANDROID_HOME="/usr/local/android-sdk" \
    ANDROID_VERSION=29 \
    ANDROID_BUILD_TOOLS_VERSION=29.0.3

ENV GOLANG_VERSION 1.13.7
```

Version references:

- Android SDK Build Tools: <https://developer.android.com/studio/releases/build-tools>
- Go versions: <https://go.dev/dl/>

If changing Go version, update the corresponding Dockerfile checksum (around legacy line ~70).

In `babble-android` (`/babble/build.gradle`), verify:

- `compileSdkVersion`
- `targetSdkVersion`

match the versions used in `babble`.

Also confirm Android SDK Build Tools installed in Android Studio (`Tools -> SDK Manager`) match Dockerfile values.

## 2) Build Mobile Distribution

From `babble` repository root:

```bash
cd docker
make mobile-image
cd ..
make mobile-dist
```

Default output naming:

- `babble_<branch-name>_<commit-hash>_android_library.zip`

To provide custom version naming:

```bash
make mobile-dist VERSION=testing
```

This generates:

- `babble_testing_android_library.zip`

Artifacts are written to:

- `./build/distmobile/`

## 3) Official Release Example

For a tagged release from master (example `v0.6.2`):

- run `make mobile-dist VERSION=v0.6.2`
- upload `build/distmobile/babble_v0.6.2_android_library.zip` to GitHub Releases
