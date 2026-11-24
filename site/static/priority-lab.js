for (const lab of document.querySelectorAll("[data-priority-lab]")) {
  const high = lab.querySelector("[name=p8]");
  const low = lab.querySelector("[name=p16]");
  const value = lab.querySelector("[data-value]");
  const source = lab.querySelector("[data-source]");

  function update() {
    value.textContent = (high.value || low.value || "20") + " °C";
    source.textContent = high.value
      ? "Priority 8"
      : low.value
        ? "Priority 16"
        : "Relinquish default";
  }

  lab.addEventListener("change", update);
  update();
  lab.hidden = false;
}
