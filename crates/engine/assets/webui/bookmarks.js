let bookmarks = [];

async function load() {
  bookmarks = (await api("state")).bookmarks;
  render();
}

function render() {
  const q = $("#filter").value.toLowerCase();
  const shown = bookmarks.filter((b) => !q || (b.title + " " + b.url).toLowerCase().includes(q)).reverse();
  $("#list").replaceChildren(...shown.map((b) =>
    el("div", { class: "card", onclick: () => { location.href = b.url; } },
      el("div", { class: "truncate" }, b.title),
      el("div", { class: "site truncate" }, host(b.url)),
      el("div", { class: "actions", style: "margin-top:10px" },
        el("button", { onclick: async (e) => {
          e.stopPropagation();
          api("openUrl", { url: b.url });
        } }, "new tab"),
        el("button", { class: "danger", onclick: async (e) => {
          e.stopPropagation();
          await api("removeBookmark", { id: b.id });
          toast("removed");
          load();
        } }, "delete")))));
  const empty = $("#empty");
  empty.hidden = shown.length > 0;
  empty.textContent = bookmarks.length ? "no matches" : "no bookmarks yet";
}

$("#filter").addEventListener("input", render);
load();
