# Security policy

## Reporting a vulnerability

Please **do not open a public issue** for security problems.
Report them privately through GitHub's [private vulnerability reporting](https://github.com/itsArial/unskilled-browser/security/advisories/new).

Include:
- what you found;
- how to reproduce it;
- the version (shown in `unskilled://settings` → about);
- your platform.

You'll get an answer within a week. Once a fix is released, you'll be credited unless you'd rather not be.

## Scope

**In scope**
- The vault: encryption, key handling, TPM sealing, session cleanup.
- The password manager and the PIN prompt.
- The sign-in gate.
- The bridge between internal `unskilled://` pages and the browser.
- Anything that leaks browsing data off the machine or between sites.
- Blocker bypasses that let a page detect or disable the blocker in a way that harms privacy.

**Out of scope**
- Chromium bugs. Report those to the [Chromium project](https://www.chromium.org/Home/chromium-security/reporting-security-bugs/).
- Releases follow upstream CEF builds.

## Supported versions

Only the latest release gets security fixes.
