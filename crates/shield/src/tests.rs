use super::*;

/// A shield over inline rules, without the background updater.
fn shield(rules: &str, trusted: bool) -> Shield {
    static LIST: lists::List = lists::List {
        id: "test",
        title: "test list",
        category: Category::Ads,
        trusted: false,
        hosts: false,
        max_age: Duration::from_secs(0),
        urls: &[],
    };
    static TRUSTED_LIST: lists::List = lists::List { trusted: true, ..LIST };
    let list = if trusted { &TRUSTED_LIST } else { &LIST };
    let (loaded, _) = build(vec![(list, rules.to_owned())]);
    Shield {
        dir: std::env::temp_dir().join(format!("unskilled-shield-test-{}", std::process::id())),
        loaded: RwLock::new(Some(Arc::new(loaded))),
        settings: Mutex::new(Settings::default()),
        settings_path: Mutex::new(None),
        status: Mutex::new(Status::Loading),
        ready: std::sync::Condvar::new(),
        update_requested: AtomicBool::new(false),
        on_change: Box::new(|| {}),
        generic_exceptions: Mutex::new(None),
    }
}

fn req<'a>(url: &'a str, top: &'a str, kind: ResourceKind) -> Request<'a> {
    Request { url, frame_url: top, top_url: top, kind, method: "GET" }
}

const TOP: &str = "https://news.example/article";

#[test]
fn blocks_with_rule_details() {
    let s = shield("||ads.example^\n", false);
    match s.check(&req("https://ads.example/banner.js", TOP, ResourceKind::Script)) {
        Verdict::Block(m) => {
            assert_eq!(m.rule, "||ads.example^");
            assert_eq!(m.list, "test list");
        }
        _ => panic!("expected block"),
    }
    assert!(matches!(s.check(&req("https://cdn.example/app.js", TOP, ResourceKind::Script)), Verdict::Allow));
}

#[test]
fn exception_rules_win() {
    let s = shield("||ads.example^\n@@||ads.example/ok.js\n", false);
    assert!(matches!(s.check(&req("https://ads.example/ok.js", TOP, ResourceKind::Script)), Verdict::Allow));
}

#[test]
fn never_blocks_top_level_documents() {
    let s = shield("||ads.example^\n", false);
    assert!(matches!(s.check(&req("https://ads.example/", "", ResourceKind::Document)), Verdict::Allow));
}

#[test]
fn redirects_to_neutered_resource() {
    let s = shield("||googletagservices.com/tag/js/gpt.js$script,redirect=googletagservices_gpt.js\n", false);
    match s.check(&req("https://www.googletagservices.com/tag/js/gpt.js", TOP, ResourceKind::Script)) {
        Verdict::Redirect { mime, body, .. } => {
            assert_eq!(mime, "application/javascript");
            assert!(!body.is_empty());
        }
        _ => panic!("expected redirect"),
    }
}

#[test]
fn strips_tracking_params() {
    let s = shield("$removeparam=utm_source\n", false);
    match s.check(&req("https://shop.example/?id=1&utm_source=mail", "", ResourceKind::Document)) {
        Verdict::Rewrite(url) => assert_eq!(url, "https://shop.example/?id=1"),
        _ => panic!("expected rewrite"),
    }
}

#[test]
fn pause_and_site_exceptions_allow_everything() {
    let s = shield("||ads.example^\nnews.example##.ad\n", false);
    let r = req("https://ads.example/x.js", TOP, ResourceKind::Script);

    s.set_paused(true);
    assert!(matches!(s.check(&r), Verdict::Allow));
    assert!(s.page_script(TOP, TOP).is_none());
    s.set_paused(false);

    s.set_excepted("https://www.news.example/", true);
    assert!(s.is_excepted("https://m.news.example/x"));
    assert!(matches!(s.check(&r), Verdict::Allow));
    assert!(s.page_script(TOP, TOP).is_none());
    s.set_excepted(TOP, false);
    assert!(matches!(s.check(&r), Verdict::Block(_)));
}

#[test]
fn page_script_carries_selectors_and_scriptlets() {
    let s = shield("news.example##.ad-slot\nnews.example##+js(set-constant, adsEnabled, false)\n", false);
    let script = s.page_script(TOP, TOP).unwrap();
    assert!(script.starts_with("// Injected"));
    assert!(script.contains("\".ad-slot\""));
    assert!(script.contains("adsEnabled"));
    assert!(script.contains("function setConstant"));
}

#[test]
fn trusted_scriptlets_need_trusted_lists() {
    let rule = "news.example##+js(trusted-replace-fetch-response, /ads/, , player)\n";
    let untrusted = shield(rule, false).page_script(TOP, TOP).unwrap();
    assert!(!untrusted.contains("function trustedReplaceFetchResponse"));
    let trusted = shield(rule, true).page_script(TOP, TOP).unwrap();
    assert!(trusted.contains("function trustedReplaceFetchResponse"));
}

#[test]
fn generic_selectors() {
    let s = shield("##.sponsored-box\n", false);
    let json = s.generic_selectors_json(TOP, &["sponsored-box".into(), "content".into()], &[]);
    assert_eq!(json, r#"[".sponsored-box"]"#);
}

#[test]
fn site_of_strips_www() {
    assert_eq!(site_of("https://WWW.YouTube.com:443/watch?v=1").as_deref(), Some("youtube.com"));
    assert_eq!(site_of("https://user@m.example.org/").as_deref(), Some("m.example.org"));
    assert_eq!(site_of("about:blank"), None);
}

/// Downloads the real lists. `cargo test -p unskilled-shield -- --ignored --nocapture`
#[test]
#[ignore]
fn real_lists_handle_youtube() {
    let dir = std::env::temp_dir().join("unskilled-shield-live");
    let texts: Vec<_> = LISTS
        .iter()
        .filter_map(|l| match lists::download(&dir, l) {
            Ok(t) => {
                println!("{:<22} {:>8} bytes", l.id, t.len());
                Some((l, t))
            }
            Err(e) => {
                println!("{:<22} FAILED {e}", l.id);
                None
            }
        })
        .collect();
    let started = Instant::now();
    let (loaded, filters) = build(texts);
    println!("{filters} filters, built in {:?}", started.elapsed());
    let s = Shield {
        dir: dir.clone(),
        loaded: RwLock::new(Some(Arc::new(loaded))),
        settings: Mutex::new(Settings::default()),
        settings_path: Mutex::new(None),
        status: Mutex::new(Status::Loading),
        ready: std::sync::Condvar::new(),
        update_requested: AtomicBool::new(false),
        on_change: Box::new(|| {}),
        generic_exceptions: Mutex::new(None),
    };

    let yt = "https://www.youtube.com/watch?v=dQw4w9WgXcQ";
    for (url, kind) in [
        ("https://securepubads.g.doubleclick.net/tag/js/gpt.js", ResourceKind::Script),
        ("https://www.youtube.com/api/stats/ads?ver=2", ResourceKind::Xhr),
        ("https://www.google-analytics.com/analytics.js", ResourceKind::Script),
        ("https://static.doubleclick.net/instream/ad_status.js", ResourceKind::Script),
    ] {
        let verdict = match s.check(&req(url, yt, kind)) {
            Verdict::Allow => "ALLOW".to_owned(),
            Verdict::Block(m) => format!("block  [{}] {}", m.list, m.rule),
            Verdict::Redirect { mime, matched, .. } => format!("redirect {mime} [{}] {}", matched.list, matched.rule),
            Verdict::Rewrite(u) => format!("rewrite {u}"),
        };
        println!("{url}\n    {verdict}");
    }
    let script = s.page_script(yt, yt).unwrap();
    println!("youtube page script: {} bytes", script.len());
    for needle in ["jsonPrune", "trustedEditInboundObject", "replaceNodeText"] {
        println!("  {needle}: {}", script.contains(needle));
    }
    assert!(script.contains("jsonPrune"));
    assert!(matches!(
        s.check(&req("https://securepubads.g.doubleclick.net/tag/js/gpt.js", yt, ResourceKind::Script)),
        Verdict::Block(_) | Verdict::Redirect { .. }
    ));
}
