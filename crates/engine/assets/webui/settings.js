let state = null;
let revealed = {};

// Password calls may need the vault PIN first: the browser asks in its own window and the page
// waits for the answer.
async function withAuth(reply) {
  if (!reply.needsAuth) return reply;
  toast("confirm with your pin in the address bar");
  for (;;) {
    await new Promise((r) => setTimeout(r, 250));
    const r = await api("authResult", { ticket: reply.ticket });
    if (!r.pending) return r;
  }
}

async function load() {
  state = await api("state");
  render();
}

function render() {
  for (const box of document.querySelectorAll("[data-pref]")) box.checked = !!state.prefs[box.dataset.pref];

  const identity = $("#identity");
  identity.replaceChildren();
  if (!state.identity.length) identity.append(el("div", { class: "empty" }, "no decisions yet — you'll be asked the first time a site wants one."));
  for (const d of state.identity) {
    identity.append(el("div", { class: "row" },
      el("div", { class: "label truncate" }, d.site, el("small", {}, "uses your " + d.providerName + " sign-in")),
      el("span", { class: "tag " + (d.decision === "allow" ? "allow" : "") }, d.decision === "allow" ? "shared" : "signed out"),
      el("div", { class: "actions" }, el("button", { onclick: async () => {
        await api("setIdentity", { site: d.site, provider: d.provider, decision: null });
        toast("forgotten — you'll be asked again"); load();
      } }, "forget"))));
  }

  renderPasswords();
  $("#bookmark-count").textContent = state.bookmarks.length + " saved";

  const select = $("#search-engine");
  select.replaceChildren(...state.engines.map((e) => el("option", { value: e.id }, e.label)));
  select.value = state.prefs.search;

  $("#vault-text").replaceChildren(
    state.vault === "tpm"
      ? "sealed in this machine's TPM and unlocked with your PIN. repeated wrong PINs lock the TPM; the vault file is useless on any other machine."
      : "protected by your passphrase (argon2id, 256 MB). " + (state.tpmAvailable ? "this machine's TPM is available: the next time you unlock, you'll be offered a PIN instead." : ""),
    el("p", { class: "notice" }, "only logins and site storage survive a restart — history, cache and every other trace is wiped each time you close the browser."));

  $("#about-text").replaceChildren("unskilled browser " + state.version + " · chromium " + state.chromium + " · GPL-3.0 · ",
    el("a", { href: "#", onclick: (e) => { e.preventDefault(); api("openUrl", { url: state.source }); } }, "source code"));
}

function renderPasswords() {
  const list = $("#password-list");
  const filter = $("#password-filter").value.toLowerCase();
  const rows = state.passwords.filter((p) => !filter || (p.origin + " " + p.username).toLowerCase().includes(filter));
  list.replaceChildren();
  if (!rows.length) list.append(el("div", { class: "empty" }, state.passwords.length ? "no matches" : "no saved passwords"));
  for (const p of rows) {
    const secret = el("span", { class: "value secret" }, revealed[p.id] || "••••••••");
    list.append(el("div", { class: "row" },
      el("div", { class: "label truncate" }, host(p.origin), el("small", {}, p.username || "(no username)")),
      secret,
      el("div", { class: "actions" },
        el("button", { onclick: async () => {
          if (revealed[p.id]) { delete revealed[p.id]; return renderPasswords(); }
          const r = await withAuth(await api("revealPassword", { id: p.id }));
          if (r.denied) return toast("cancelled");
          revealed[p.id] = r.password; renderPasswords();
        } }, revealed[p.id] ? "hide" : "show"),
        el("button", { onclick: async () => {
          const r = await withAuth(await api("copyPassword", { id: p.id }));
          toast(r.denied ? "cancelled" : "copied — clears in 30 s");
        } }, "copy"),
        el("button", { class: "danger", onclick: async () => {
          await api("removePassword", { id: p.id }); toast("removed"); load();
        } }, "delete"))));
  }
}

for (const box of document.querySelectorAll("[data-pref]")) {
  box.addEventListener("change", async () => {
    state.prefs[box.dataset.pref] = box.checked;
    const r = await withAuth(await api("setPrefs", { prefs: state.prefs }));
    if (r.denied) { toast("cancelled"); return load(); }
    toast("saved");
  });
}
$("#search-engine").addEventListener("change", async (e) => {
  state.prefs.search = e.target.value;
  await api("setPrefs", { prefs: state.prefs });
  toast("search engine changed");
});
$("#password-filter").addEventListener("input", renderPasswords);
$("#import-passwords").addEventListener("click", async () => {
  const text = await readFile(".csv,text/csv");
  if (text == null) return;
  const r = await api("importPasswords", { text });
  toast(r.error ? r.error : r.added + " passwords imported");
  load();
});
$("#import-bookmarks").addEventListener("click", async () => {
  const text = await readFile(".html,.htm,text/html");
  if (text == null) return;
  const r = await api("importBookmarks", { text });
  toast(r.added + " bookmarks imported");
  load();
});
for (const a of document.querySelectorAll("nav a")) {
  a.addEventListener("click", () => {
    document.querySelectorAll("nav a").forEach((x) => x.classList.toggle("active", x === a));
  });
}
load();
