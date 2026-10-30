let state = { extensions: [], restartNeeded: false };

async function load() {
  state = await api("extensions");
  render();
}

function render() {
  $("#restart").hidden = !state.restartNeeded;
  const list = $("#list");
  list.replaceChildren();
  if (!state.extensions.length) list.append(el("div", { class: "empty" }, "no extensions yet"));
  for (const e of state.extensions) {
    const toggle = el("input", { type: "checkbox" });
    toggle.checked = e.enabled;
    toggle.addEventListener("change", async () => {
      state = await api("setExtensionEnabled", { id: e.id, enabled: toggle.checked });
      render();
    });
    list.append(el("div", { class: "row extension" + (e.enabled ? "" : " off") },
      e.icon ? el("img", { class: "icon", src: e.icon, alt: "" }) : el("div", { class: "icon blank" }),
      el("div", { class: "label" },
        el("div", { class: "truncate" }, e.name, el("span", { class: "version" }, " " + e.version)),
        el("small", { class: "truncate" }, e.description || "—"),
        el("small", { class: "id" }, e.id)),
      el("div", { class: "actions" },
        el("label", { class: "switch" }, toggle, el("span")),
        el("button", { class: "danger", onclick: async () => {
          state = await api("removeExtension", { id: e.id });
          toast(e.name + " removed");
          render();
        } }, "remove"))));
  }
}

async function install(file) {
  if (!file) return;
  toast("installing " + file.name + "…");
  const data = await new Promise((resolve) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result).split(",")[1] || "");
    reader.readAsDataURL(file);
  });
  const r = await api("installExtension", { data });
  if (r.error) return toast(r.error);
  state = r.state;
  toast(r.name + " added");
  render();
}

$("#pick").addEventListener("click", () => {
  const input = el("input", { type: "file", accept: ".crx,.zip" });
  input.addEventListener("change", () => install(input.files[0]));
  input.click();
});
const drop = $("#drop");
drop.addEventListener("dragover", (e) => { e.preventDefault(); drop.classList.add("over"); });
drop.addEventListener("dragleave", () => drop.classList.remove("over"));
drop.addEventListener("drop", (e) => { e.preventDefault(); drop.classList.remove("over"); install(e.dataTransfer.files[0]); });

$("#store-add").addEventListener("click", async () => {
  const url = $("#store-url").value.trim();
  if (!url) return;
  toast("downloading from the web store…");
  const r = await api("installFromStore", { url });
  if (r.error) return toast(r.error);
  // The download finishes in the background; poll until the extension shows up.
  for (let i = 0; i < 120; i++) {
    await new Promise((res) => setTimeout(res, 500));
    const s = await api("storeResult", { ticket: r.ticket });
    if (s.pending) continue;
    if (s.error) return toast(s.error);
    $("#store-url").value = "";
    toast(s.name + " added");
    return load();
  }
  toast("the download timed out");
});
$("#restart-now").addEventListener("click", () => api("restart"));
load();
