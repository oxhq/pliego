# Hybrid Canvas prototype

The retained vector-safe subset is intentionally one command: solid-color `fillRect` with the
identity transform, source-over composition, and no clip, shadow, or filter. `raster_patch` carries
the already-computed premultiplied RGBA pixels for one pixel-dependent region; Pliego places only
that patch as an image and reports its typed reason, bounds, and pixel area.

This is an adapter fixture, not live browser Canvas capture. Servo's serializable `CanvasCommand`
stream is consumed by `CanvasPaintThread`; layout retains only a runtime `ImageKey`, so Pliego
cannot yet recover the commands or pixels from `Layout::debug_snapshot`. A future bridge must
retain the live commands before this adapter can be connected. WebGL, text, transforms, gradients,
clipping, shadows, compositing, and general filters are unsupported here. The prototype uses one
scene unit per patch pixel; scaled patches are left for the live bridge.
