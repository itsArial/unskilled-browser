//! Windows: the bundle's `unskilled.exe` is CEF's prebuilt bootstrap.exe with our icon and version
//! info in place of CEF's (Explorer, the taskbar and Task Manager read them from the .exe, not the
//! DLL). Written to `$OUT_DIR/unskilled.exe`, where scripts/package.sh picks it up. The DLL gets
//! the same resources.
//!
//! The DLL is linked like CEF links its own client binaries, with most system DLLs delay-loaded:
//! every sandboxed sub-process loads it, and renderers must not load user32/gdi32 (win32k is off
//! for them). Only the browser process, which runs the UI, ever calls into those.

/// CEF's `CEF_DELAYLOAD_FLAGS` (cmake/cef_variables.cmake.in, CEF 156), kept in sync with
/// Chromium's //build/config/win/BUILD.gn. Ones we don't import are ignored by the linker.
const DELAY_LOADS: &[&str] = &[
    "libcef.dll",
    "api-ms-win-core-synch-l1-2-0.dll",
    "api-ms-win-core-winrt-error-l1-1-0.dll",
    "api-ms-win-core-winrt-l1-1-0.dll",
    "api-ms-win-core-winrt-string-l1-1-0.dll",
    "advapi32.dll",
    "bcryptprimitives.dll",
    "comctl32.dll",
    "comdlg32.dll",
    "credui.dll",
    "cryptui.dll",
    "d3d11.dll",
    "d3d12.dll",
    "d3d9.dll",
    "dbghelp.dll",
    "dcomp.dll",
    "dwmapi.dll",
    "dxgi.dll",
    "dxva2.dll",
    "esent.dll",
    "fontsub.dll",
    "gdi32.dll",
    "hid.dll",
    "imagehlp.dll",
    "imm32.dll",
    "mmdevapi.dll",
    "msi.dll",
    "netapi32.dll",
    "ncrypt.dll",
    "ole32.dll",
    "oleacc.dll",
    "oleaut32.dll",
    "pdh.dll",
    "propsys.dll",
    "psapi.dll",
    "rpcrt4.dll",
    "rstrtmgr.dll",
    "setupapi.dll",
    "shell32.dll",
    "shlwapi.dll",
    "uiautomationcore.dll",
    "urlmon.dll",
    "user32.dll",
    "usp10.dll",
    "uxtheme.dll",
    "wer.dll",
    "wevtapi.dll",
    "wininet.dll",
    "winusb.dll",
    "wsock32.dll",
    "wtsapi32.dll",
    "crypt32.dll",
    "dhcpcsvc.dll",
    "dwrite.dll",
    "iphlpapi.dll",
    "secur32.dll",
    "userenv.dll",
    "winhttp.dll",
    "winmm.dll",
    "winspool.drv",
    "wintrust.dll",
    "ws2_32.dll",
];

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=build.rs");
        for dll in DELAY_LOADS {
            println!("cargo:rustc-cdylib-link-arg=/DELAYLOAD:{dll}");
        }
        println!("cargo:rustc-link-lib=delayimp");
        bootstrap::brand();
    }
}

#[cfg(windows)]
mod bootstrap {
    use std::{collections::BTreeSet, env, fs, path::PathBuf};
    use windows_sys::Win32::{
        Foundation::{FALSE, FreeLibrary, HMODULE, TRUE},
        System::LibraryLoader::{
            BeginUpdateResourceW, EndUpdateResourceW, EnumResourceLanguagesW, EnumResourceNamesW,
            LOAD_LIBRARY_AS_DATAFILE, LOAD_LIBRARY_AS_IMAGE_RESOURCE, LoadLibraryExW, UpdateResourceW,
        },
    };

    const ICON: &str = "../../assets/icons/unskilled-browser.ico";
    /// IDI_APPLICATION, the id CEF gives the bootstrap's own icon.
    const ICON_ID: &str = "32512";

    pub fn brand() {
        let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
        let cef = PathBuf::from(env::var_os("DEP_CEF_DLL_WRAPPER_CEF_DIR").expect("CEF directory from cef-dll-sys"));
        let source = cef.join("bootstrap.exe");
        println!("cargo:rerun-if-changed={ICON}");
        println!("cargo:rerun-if-changed={}", source.display());

        let mut res = winresource::WindowsResource::new();
        res.set_icon_with_id(ICON, ICON_ID)
            .set_language(0x0409)
            .set("FileDescription", "unskilled browser")
            .set("ProductName", "unskilled browser")
            .set("InternalName", "unskilled")
            .set("OriginalFilename", "unskilled.exe");
        res.compile().expect("windows resources");
        let ours = parse_res(&fs::read(out.join("main_res.res")).expect("compiled resources"));

        let exe = out.join("unskilled.exe");
        let bytes = fs::read(&source).expect("CEF's bootstrap.exe");
        fs::write(&exe, &bytes).expect("write unskilled.exe");
        // Changing a signed bootstrap would break its signature, and it refuses to start then.
        if has_signature(&bytes) {
            println!("cargo:warning=bootstrap.exe is code-signed; unskilled.exe keeps CEF's icon and name");
            return;
        }
        replace_resources(&wide(&exe.to_string_lossy()), &ours);
    }

    /// A resource type or name: a number, or a NUL-terminated UTF-16 string.
    #[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
    enum Id {
        Num(u16),
        Str(Vec<u16>),
    }

    impl Id {
        /// The PCWSTR form the resource APIs take (`MAKEINTRESOURCE` for numbers).
        fn as_ptr(&self) -> *const u16 {
            match self {
                Id::Num(n) => *n as usize as *const u16,
                Id::Str(s) => s.as_ptr(),
            }
        }

        /// SAFETY: `p` is a PCWSTR the resource APIs handed us.
        unsafe fn from_ptr(p: *const u16) -> Self {
            if (p as usize) >> 16 == 0 {
                return Id::Num(p as usize as u16);
            }
            let len = (0..).take_while(|&i| unsafe { *p.add(i) } != 0).count();
            let mut s = unsafe { std::slice::from_raw_parts(p, len) }.to_vec();
            s.push(0);
            Id::Str(s)
        }
    }

    struct Resource {
        kind: Id,
        name: Id,
        lang: u16,
        data: Vec<u8>,
    }

    /// Entries of a compiled `.res` file (a sequence of RESOURCEHEADER + data, DWORD-aligned).
    fn parse_res(bytes: &[u8]) -> Vec<Resource> {
        let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
        let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let read_id = |at: &mut usize| {
            if u16_at(*at) == 0xFFFF {
                *at += 4;
                return Id::Num(u16_at(*at - 2));
            }
            let mut s = Vec::new();
            loop {
                let c = u16_at(*at);
                *at += 2;
                s.push(c);
                if c == 0 {
                    return Id::Str(s);
                }
            }
        };
        let mut resources = Vec::new();
        let mut at = 0;
        while at + 8 <= bytes.len() {
            let (data_size, header_size) = (u32_at(at), u32_at(at + 4));
            let mut p = at + 8;
            let kind = read_id(&mut p);
            let name = read_id(&mut p);
            // Then DataVersion (4), MemoryFlags (2), LanguageId (2), Version, Characteristics.
            let lang = u16_at(p.next_multiple_of(4) + 6);
            let data = at + header_size;
            // The file opens with an empty entry of type 0.
            if kind != Id::Num(0) {
                resources.push(Resource { kind, name, lang, data: bytes[data..data + data_size].to_vec() });
            }
            at = (data + data_size).next_multiple_of(4);
        }
        resources
    }

    /// Whether the PE has an Authenticode signature (a certificate table).
    fn has_signature(pe: &[u8]) -> bool {
        let u32_at = |at: usize| u32::from_le_bytes(pe[at..at + 4].try_into().unwrap()) as usize;
        let optional = u32_at(0x3C) + 24;
        // The security directory is data directory 4; they start at 96 (PE32) or 112 (PE32+).
        let directories = optional + if pe[optional..optional + 2] == [0x0B, 0x02] { 112 } else { 96 };
        u32_at(directories + 4 * 8 + 4) != 0
    }

    /// Every (type, name, language) in `exe` of the given types.
    fn existing(exe: &[u16], kinds: &BTreeSet<Id>) -> Vec<(Id, Id, u16)> {
        unsafe extern "system" fn on_name(_: HMODULE, _: *const u16, name: *const u16, names: isize) -> i32 {
            unsafe { (*(names as *mut Vec<Id>)).push(Id::from_ptr(name)) };
            TRUE
        }
        unsafe extern "system" fn on_lang(_: HMODULE, _: *const u16, _: *const u16, lang: u16, langs: isize) -> i32 {
            unsafe { (*(langs as *mut Vec<u16>)).push(lang) };
            TRUE
        }
        let mut found = Vec::new();
        // SAFETY: loads the file as data only; the callbacks get pointers to our own vectors.
        unsafe {
            let module = LoadLibraryExW(exe.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE);
            assert!(!module.is_null(), "load bootstrap.exe: {}", std::io::Error::last_os_error());
            for kind in kinds {
                let mut names: Vec<Id> = Vec::new();
                EnumResourceNamesW(module, kind.as_ptr(), Some(on_name), &mut names as *mut _ as isize);
                for name in names {
                    let mut langs: Vec<u16> = Vec::new();
                    EnumResourceLanguagesW(module, kind.as_ptr(), name.as_ptr(), Some(on_lang), &mut langs as *mut _ as isize);
                    found.extend(langs.into_iter().map(|lang| (kind.clone(), name.clone(), lang)));
                }
            }
            FreeLibrary(module);
        }
        found
    }

    /// Replaces every resource of the types in `ours` (icons, version info) with ours, keeping the
    /// rest (manifest, error strings).
    fn replace_resources(exe: &[u16], ours: &[Resource]) {
        let kinds: BTreeSet<Id> = ours.iter().map(|r| r.kind.clone()).collect();
        let theirs = existing(exe, &kinds);
        let check = |ok: i32, what: &str| assert!(ok != FALSE, "{what}: {}", std::io::Error::last_os_error());
        // SAFETY: every pointer is a live buffer or a MAKEINTRESOURCE value for the duration of the call.
        unsafe {
            let update = BeginUpdateResourceW(exe.as_ptr(), FALSE);
            assert!(!update.is_null(), "open unskilled.exe: {}", std::io::Error::last_os_error());
            for (kind, name, lang) in &theirs {
                check(UpdateResourceW(update, kind.as_ptr(), name.as_ptr(), *lang, std::ptr::null(), 0), "remove resource");
            }
            for r in ours {
                let data = r.data.as_ptr().cast();
                check(UpdateResourceW(update, r.kind.as_ptr(), r.name.as_ptr(), r.lang, data, r.data.len() as u32), "add resource");
            }
            check(EndUpdateResourceW(update, FALSE), "write unskilled.exe");
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }
}

#[cfg(not(windows))]
mod bootstrap {
    pub fn brand() {}
}
