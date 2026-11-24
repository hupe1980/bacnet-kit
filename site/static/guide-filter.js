const filter = document.querySelector("#guide-filter");
const groups = [...document.querySelectorAll(".guide-group")];
const empty = document.querySelector(".empty");
function update() {
  const terms = filter.value.toLowerCase().match(/[a-z0-9_]+/g) ?? [];
  let visible = 0;
  for (const group of groups) {
    let hits = 0;
    for (const row of group.querySelectorAll(".guide-row")) {
      const words = (row.textContent + " " + row.dataset.keywords)
        .toLowerCase()
        .split(/[^a-z0-9_]+/);
      row.hidden = !terms.every((term) =>
        words.some((word) => word.startsWith(term)),
      );
      if (!row.hidden) hits++;
    }
    group.hidden = hits === 0;
    visible += hits;
  }
  empty.hidden = visible !== 0;
}
filter.closest("label").hidden = false;
filter.addEventListener("input", update);
update();
