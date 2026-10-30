// Login forms: tells the browser when a page shows one (so it can offer saved logins) and when a
// login is submitted (so it can offer to save it). Fills only on the browser's command, after the
// user picked a login. Evaluated natively at document start; nothing is exposed on `window`.
(function (report) {
    'use strict';
    const doc = document;
    const valueSetter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
    const visible = (el) => {
        const r = el.getBoundingClientRect();
        return r.width > 0 && r.height > 0;
    };
    const passwords = (scope) => [...(scope || doc).querySelectorAll('input[type=password]')].filter(visible);
    const usernameFor = (pw) => {
        const scope = pw.form || doc;
        const inputs = [...scope.querySelectorAll('input')];
        for (let i = inputs.indexOf(pw) - 1; i >= 0; i--) {
            const e = inputs[i];
            if (/^(text|email|tel)?$/.test(e.type) && visible(e)) { return e; }
        }
        return scope.querySelector('input[autocomplete~=username], input[type=email]');
    };

    let last = '';
    const capture = (pw) => {
        if (!pw || !pw.value) { return; }
        const user = usernameFor(pw);
        const key = (user ? user.value : '') + '\u0000' + pw.value;
        if (key === last) { return; }
        last = key;
        report('submit', JSON.stringify({ username: user ? user.value : '', password: pw.value }));
    };
    doc.addEventListener('submit', (e) => capture(e.target.querySelector && e.target.querySelector('input[type=password]')), true);
    doc.addEventListener('keydown', (e) => {
        const t = e.target;
        if (e.key !== 'Enter' || !(t instanceof HTMLInputElement)) { return; }
        capture(t.type === 'password' ? t : t.form && t.form.querySelector('input[type=password]'));
    }, true);
    doc.addEventListener('click', (e) => {
        const b = e.target.closest && e.target.closest('button, input[type=submit], [role=button]');
        if (!b) { return; }
        const scope = b.form || b.closest('form') || doc;
        capture([...scope.querySelectorAll('input[type=password]')].find((p) => p.value));
    }, true);

    let announced = false;
    const check = () => {
        if (!announced && passwords().length) {
            announced = true;
            report('form', '');
        }
    };
    const start = () => {
        check();
        new MutationObserver(() => { if (!announced) { check(); } }).observe(doc.documentElement, { childList: true, subtree: true });
    };
    if (doc.readyState === 'loading') { doc.addEventListener('DOMContentLoaded', start, { once: true }); } else { start(); }

    const set = (el, value) => {
        el.focus();
        // The native setter, so frameworks that track input values (React, Vue) notice.
        valueSetter.call(el, value);
        el.dispatchEvent(new Event('input', { bubbles: true }));
        el.dispatchEvent(new Event('change', { bubbles: true }));
    };
    return function fill(username, password) {
        const pw = passwords()[0];
        if (!pw) { return; }
        const user = usernameFor(pw);
        if (user && username) { set(user, username); }
        set(pw, password);
    };
})
