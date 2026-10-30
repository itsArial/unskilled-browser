// Shared by the internal pages. `window.unskilled.call` is a bridge to the browser that only
// exists on unskilled:// pages; the browser re-checks the page's origin on every call.
const api = (method, params) => window.unskilled.call(method, params || {});

const $ = (sel, root = document) => root.querySelector(sel);
const el = (tag, attrs = {}, ...children) => {
  const node = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === "class") node.className = v;
    else if (k.startsWith("on")) node.addEventListener(k.slice(2), v);
    else node.setAttribute(k, v);
  }
  for (const c of children.flat()) node.append(c instanceof Node ? c : document.createTextNode(String(c)));
  return node;
};

function toast(text) {
  let t = $(".toast");
  if (!t) document.body.append(t = el("div", { class: "toast" }));
  t.textContent = text;
  t.classList.add("show");
  clearTimeout(t.timer);
  t.timer = setTimeout(() => t.classList.remove("show"), 2200);
}

function readFile(accept) {
  return new Promise((resolve) => {
    const input = el("input", { type: "file", accept });
    input.addEventListener("change", () => {
      const file = input.files[0];
      if (!file) return resolve(null);
      const reader = new FileReader();
      reader.onload = () => resolve(reader.result);
      reader.readAsText(file);
    });
    input.click();
  });
}

function host(url) {
  try { return new URL(url).host.replace(/^www\./, ""); } catch { return url; }
}
