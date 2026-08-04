queueMicrotask(() => {
  document.querySelector("#result").textContent = "ready";
  document.body.dataset.sessionState = "ready";
  console.info("pliego fixture ready");
});
