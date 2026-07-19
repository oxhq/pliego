# Hybrid Canvas prototype

The retained vector-safe subset is intentionally one command: solid-color `fillRect` with the
identity transform, source-over composition, and no clip, shadow, or filter. `raster_patch` carries
the already-computed premultiplied RGBA pixels for one pixel-dependent region; Pliego places only
that patch as an image and reports its typed reason, bounds, and pixel area.

`live.html` proves the matching browser path: JavaScript commands are retained at
`CanvasPaintThread`, joined to the laid-out element by its runtime `ImageKey`, and scaled into the
document scene. WebGL, text, transforms, gradients, clipping, shadows, compositing, and general
filters remain typed unsupported commands. Pixel readback is retained only as its bounded RGBA
patch; the normal Servo Canvas raster and continuous layout behavior are unchanged.
