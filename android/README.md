# Worth for Android

This is the same Hotwire Native shell used by the Rails app, pointed at Rust on
`http://localhost:3001`. The app remains named **Worth Dev** for local debug builds
and **Worth** for release builds. Debug package ID `app.worth.mobile.rustdebug`
lets it coexist with the Rails debug app (`app.worth.mobile.debug`).

Start the server with `bin/dev`. With JDK 17 and Android SDK platform 36/build tools
36.0.0/platform-tools installed, set `ANDROID_HOME` to your SDK and run:

```sh
bin/android build
bin/android check
bin/android devices
bin/android install
```

`install` forwards the device's port 3001 to your Mac and installs the debug APK.
Pass a device serial if more than one device is connected. The APK is written to
`tmp/android-builds/Worth-debug.apk`. Run `bin/android connect` to restore forwarding.

The shell uses the Rust app's accounts and data. Sign in with the same credentials
as the Rust web app. Browser sessions and native sessions are separate. Bank linking
and CSV downloads open in a browser tab. Files can be uploaded through Android's
file picker. Read-only bank requests, authentication and encryption remain on the
server; credentials are never bundled into the APK.

Hosted build: `bin/android build -PworthBaseUrl=https://your-worth-host.example`.
Release variants require HTTPS; release signing and Play Store publication are
separate work. Native Plaid/OAuth handling, push, biometrics and offline operation
remain outside this prototype.
