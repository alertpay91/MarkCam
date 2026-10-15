# macos-cmio

macOS virtual-camera consumer: a **CoreMediaIO camera extension** (`CMIOExtension`,
the modern replacement for deprecated DAL plugins), packaged inside the app bundle and
activated via `SystemExtensions`.

It maps the `openvcam.<session>.video` ring, reads frames with the seqlock, and vends them
through a `CMIOExtensionStream` so apps see **"OpenVCam"** as an `AVCaptureDevice`.

Build: Swift Package / Xcode, codesigned + notarized for distribution.
