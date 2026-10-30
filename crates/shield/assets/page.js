// Injected natively at V8 context creation, before any page script runs.
// Called with `query`, a native function that is never reachable from the page.
(function (query) {
    'use strict';
    const SELECTORS = __SELECTORS__;
    const PROCEDURAL = __PROCEDURAL__;
    const GENERIC = __GENERIC__;

    // Capture what we use before the page (or its anti-adblock code) can patch it.
    const doc = document;
    const win = window;
    const setTimeout_ = win.setTimeout.bind(win);
    const MutationObserver_ = win.MutationObserver;
    const CSSStyleSheet_ = win.CSSStyleSheet;
    const JSON_parse = JSON.parse;
    const HIDE = '{display:none!important}';

    // 1. Scriptlets first: they must patch globals before the page reads them.
    try {
        (function () {
__SCRIPTLETS__
        })();
    } catch (e) { }

    // 2. Hiding via a constructed stylesheet: not a <style> in the DOM, not in document.styleSheets.
    let sheet = null;
    const attach = () => {
        if (sheet === null) { return; }
        try {
            const current = doc.adoptedStyleSheets;
            if (!current.includes(sheet)) { doc.adoptedStyleSheets = [...current, sheet]; }
        } catch (e) { }
    };
    const insert = (rule) => {
        if (sheet === null) {
            try { sheet = new CSSStyleSheet_(); } catch (e) { return false; }
            attach();
        }
        try { sheet.insertRule(rule, sheet.cssRules.length); return true; } catch (e) { return false; }
    };
    const hide = (selectors) => {
        // Chunked for speed; an invalid selector invalidates its chunk, so retry those one by one.
        for (let i = 0; i < selectors.length; i += 64) {
            const chunk = selectors.slice(i, i + 64);
            if (!insert(chunk.join(',') + HIDE)) {
                for (const s of chunk) { insert(s + HIDE); }
            }
        }
    };
    hide(SELECTORS);

    // 3. Procedural filters (:has-text, :upward, :remove(), :style(), ...).
    const textOf = (el) => el.textContent || '';
    const toRegex = (arg) => {
        const m = /^\/(.+)\/([imsu]*)$/.exec(arg);
        if (m !== null) { try { return new RegExp(m[1], m[2]); } catch (e) { return null; } }
        return null;
    };
    const textMatcher = (arg) => {
        const re = toRegex(arg);
        return re !== null ? (s) => re.test(s) : (s) => s.includes(arg);
    };
    const operators = {
        'css-selector': (nodes, arg, first) => {
            if (first) { try { return [...doc.querySelectorAll(arg)]; } catch (e) { return []; } }
            // Later steps continue from each node: `> x` children, `+ x` next sibling, `~ x` following siblings.
            const out = [];
            const rest = arg.slice(1).trim();
            for (const n of nodes) {
                try {
                    if (arg[0] === '>') {
                        out.push(...n.querySelectorAll(':scope ' + arg));
                    } else if (arg[0] === '+') {
                        const next = n.nextElementSibling;
                        if (next && next.matches(rest)) { out.push(next); }
                    } else if (arg[0] === '~') {
                        for (let s = n.nextElementSibling; s; s = s.nextElementSibling) {
                            if (s.matches(rest)) { out.push(s); }
                        }
                    } else {
                        out.push(...n.querySelectorAll(arg));
                    }
                } catch (e) { }
            }
            return out;
        },
        'has-text': (nodes, arg) => { const t = textMatcher(arg); return nodes.filter(n => t(textOf(n))); },
        'min-text-length': (nodes, arg) => { const n_ = parseInt(arg, 10) || 0; return nodes.filter(n => textOf(n).length >= n_); },
        'matches-path': (nodes, arg) => { const t = textMatcher(arg); return t(location.pathname + location.search) ? nodes : []; },
        'matches-attr': (nodes, arg) => {
            const m = /^"?([^"=]+)"?(?:="?(.*?)"?)?$/.exec(arg);
            if (m === null) { return []; }
            const nameT = textMatcher(m[1]);
            const valueT = m[2] !== undefined ? textMatcher(m[2]) : null;
            return nodes.filter(n => [...n.attributes].some(a => nameT(a.name) && (valueT === null || valueT(a.value))));
        },
        'matches-css': (nodes, arg, first, pseudo) => {
            const i = arg.indexOf(':');
            if (i < 0) { return []; }
            const prop = arg.slice(0, i).trim();
            const t = textMatcher(arg.slice(i + 1).trim());
            return nodes.filter(n => t(win.getComputedStyle(n, pseudo || null).getPropertyValue(prop)));
        },
        'matches-css-before': (nodes, arg, first) => operators['matches-css'](nodes, arg, first, '::before'),
        'matches-css-after': (nodes, arg, first) => operators['matches-css'](nodes, arg, first, '::after'),
        'upward': (nodes, arg) => {
            const out = [];
            const steps = parseInt(arg, 10);
            for (const n of nodes) {
                let p = n;
                if (!isNaN(steps) && String(steps) === arg.trim()) {
                    for (let i = 0; i < steps && p; i++) { p = p.parentElement; }
                } else {
                    p = n.parentElement ? n.parentElement.closest(arg) : null;
                }
                if (p) { out.push(p); }
            }
            return out;
        },
        'xpath': (nodes, arg, first) => {
            const out = [];
            const roots = first ? [doc] : nodes;
            for (const r of roots) {
                try {
                    const res = doc.evaluate(arg, r, null, XPathResult.ORDERED_NODE_SNAPSHOT_TYPE, null);
                    for (let i = 0; i < res.snapshotLength; i++) {
                        const n = res.snapshotItem(i);
                        if (n.nodeType === 1) { out.push(n); }
                    }
                } catch (e) { }
            }
            return out;
        },
    };
    const procedural = [];
    for (const raw of PROCEDURAL) {
        try {
            const f = JSON_parse(raw);
            if (f.selector.every(op => operators[op.type] !== undefined)) { procedural.push(f); }
        } catch (e) { }
    }
    const applied = new WeakSet();
    const runProcedural = () => {
        for (const f of procedural) {
            let nodes = [];
            f.selector.forEach((op, i) => { nodes = operators[op.type](nodes, op.arg, i === 0); });
            for (const n of new Set(nodes)) {
                const a = f.action;
                if (!a) {
                    if (!applied.has(n)) { applied.add(n); n.style.setProperty('display', 'none', 'important'); }
                } else if (a.type === 'remove') {
                    n.remove();
                } else if (a.type === 'style') {
                    if (!applied.has(n)) { applied.add(n); n.style.cssText += ';' + a.arg; }
                } else if (a.type === 'remove-attr') {
                    n.removeAttribute(a.arg);
                } else if (a.type === 'remove-class') {
                    n.classList.remove(a.arg);
                }
            }
        }
    };

    // 4. Generic cosmetic filters: ask for hide rules matching classes/ids as they appear.
    const seenClasses = new Set();
    const seenIds = new Set();
    let newClasses = [];
    let newIds = [];
    const collect = (el) => {
        const id = el.id;
        if (typeof id === 'string' && id !== '' && !seenIds.has(id)) { seenIds.add(id); newIds.push(id); }
        const list = el.classList;
        if (list) {
            for (const c of list) {
                if (!seenClasses.has(c)) { seenClasses.add(c); newClasses.push(c); }
            }
        }
    };
    const scan = (node) => {
        if (node.nodeType !== 1) { return; }
        collect(node);
        for (const el of node.querySelectorAll('[id],[class]')) { collect(el); }
    };

    let scheduled = false;
    const flush = () => {
        scheduled = false;
        attach();
        if (procedural.length !== 0) { runProcedural(); }
        if (!GENERIC || (newClasses.length === 0 && newIds.length === 0)) { return; }
        const classes = newClasses;
        const ids = newIds;
        newClasses = [];
        newIds = [];
        query(classes, ids, (json) => {
            try { hide(JSON_parse(json)); } catch (e) { }
        });
    };
    const schedule = () => {
        if (!scheduled) { scheduled = true; setTimeout_(flush, 40); }
    };

    if (!GENERIC && procedural.length === 0) { return; }
    const observer = new MutationObserver_((records) => {
        for (const r of records) {
            if (r.type === 'attributes') { collect(r.target); continue; }
            for (const n of r.addedNodes) { scan(n); }
        }
        schedule();
    });
    const options = { childList: true, subtree: true };
    if (GENERIC) { options.attributeFilter = ['class', 'id']; }
    observer.observe(doc, options);
    if (doc.documentElement) { scan(doc.documentElement); }
    schedule();
})
