const builder = document.querySelector("[data-batch-builder]");
if (builder) {
  const update = () => {
    const references = Array.from(
      builder.querySelectorAll("input[value]:checked"),
      (input) => input.value,
    );
    const options = builder.querySelector("[data-peer-limits]").checked
      ? ["--device", "123"]
      : [];
    builder.querySelector("textarea").value = references.length
      ? [
          "bacnet-kit batch",
          ...options,
          "127.0.0.1:47808",
          ...references,
        ].join(" ")
      : "";
    builder.querySelector("output").textContent = references.length
      ? `${references.length} ${references.length === 1 ? "property" : "properties"} in one request${options.length ? " after reading peer limits" : ""}`
      : "Select a property to build a command.";
  };
  builder.addEventListener("change", update);
  update();
  builder.hidden = false;
}
