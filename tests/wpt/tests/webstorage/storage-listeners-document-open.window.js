promise_test(async t => {
  const frame = document.createElement("iframe");
  document.body.append(frame);
  t.add_cleanup(() => frame.remove());

  const target = frame.contentWindow;
  let inline_calls = 0;
  let first_additive_calls = 0;
  let second_additive_calls = 0;
  target.onstorage = () => inline_calls++;
  target.addEventListener("storage", () => first_additive_calls++);
  target.addEventListener("storage", () => second_additive_calls++);

  // document.open() erases every listener on the associated Window. Servo also uses that exact
  // listener lifecycle as the authoritative storage-event interest/source count.
  target.document.open();
  target.document.write("<!doctype html><title>replacement</title>");
  target.document.close();
  assert_equals(target.onstorage, null, "the inline storage handler was cleared");

  let resolve_fresh_event;
  const fresh_event = new Promise(resolve => {
    resolve_fresh_event = resolve;
  });
  const fresh_listener = () => resolve_fresh_event();
  target.addEventListener("storage", fresh_listener, {once: true});

  const key = `document-open-storage-listeners-${token()}`;
  t.add_cleanup(() => localStorage.removeItem(key));
  localStorage.setItem(key, "value");

  await Promise.race([
    fresh_event,
    new Promise((_, reject) => {
      t.step_timeout(() => reject(new Error("timed out waiting for storage event")), 2000);
    }),
  ]);

  assert_equals(inline_calls, 0, "the erased inline handler did not run");
  assert_equals(first_additive_calls, 0, "the first erased additive listener did not run");
  assert_equals(second_additive_calls, 0, "the second erased additive listener did not run");
}, "document.open() clears onstorage and every additive storage listener");
