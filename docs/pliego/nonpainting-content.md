# Candidate nonpainting-content capture

The 0.4 development line treats two barcode-generator patterns as nonpainting,
without changing the application's generated HTML:

- An ordinary solid-color background with a valid, exactly zero-width or
  zero-height original app-unit rectangle emits no PDF path.
- A text fragment whose actual font size is exactly zero emits no PDF text
  operation. Its layout remains present, and positive-size descendants still paint.

This is not a blanket invisible-content filter. A positive one-app-unit extent is
not zero, negative/overflowing rectangles are not accepted, and rounded float
coordinates do not establish emptiness. The empty-background case still requires
the existing ordinary solid border-box, axis-aligned, untransformed/unclipped
geometry path. Background images, overrides, border/shadow/effect exclusions and
the text-effect/unresolved-counter checks remain active. The normal Servo display
list and layout are unchanged; only absent ink is omitted from the captured PDF
paint operations.

This boundary came from the original Aureus manufacturing PDF barcode markup:
six zero-width terminal black boxes and zero-font-size nonbreaking spaces. The
retained candidate census failed those cases before implementation. Removing the
boxes or spaces from the application's template was not the fix.

Contributor checks:

```sh
python ports/pliego/tests/check_api2_nonpainting_content.py --self-test
python ports/pliego/tests/check_api2_nonpainting_content.py \
  --binary /absolute/path/to/pliego --source-commit FULL_SOURCE_SHA \
  --out /absolute/fresh/proof-directory --require-pdf-text
```

The twelve native cases retain a visible 1.2px barcode-style bar and `KEEP` control,
zero/positive width with empty/NBSP content, zero height, a positive-font descendant,
and explicit decoration/shadow/background-image rejections. Positive paint must
keep its exact source geometry, font and order. Corruption checks reject lost bars,
lost text, zero-size operations, wrong color/font and wrong glyph geometry.

This focused regression is not full barcode reliability, a released capability,
or business-document qualification. The original work order must still pass its
three independent Code128 decodes, quantity/order/geometry checks, visual review,
and storage/readback against the exact candidate package before comparison timing.
