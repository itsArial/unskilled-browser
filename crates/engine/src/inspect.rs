//! Per-tab network record from Chromium's own DevTools protocol: for every request, the IP and
//! port the browser actually connected to, the protocol (h2/h3), TLS details and headers.

use cef::*;
use serde_json::Value;
use std::{cell::RefCell, collections::HashMap, rc::Rc};

const LIMIT: usize = 3000;

#[derive(Clone, Debug, Default)]
pub struct Tls {
    pub protocol: String,
    pub cipher: String,
    pub key_exchange: String,
    pub subject: String,
    pub issuer: String,
    /// Unix seconds.
    pub valid_to: i64,
}

#[derive(Clone, Debug, Default)]
pub struct Request {
    pub url: String,
    pub host: String,
    /// CDP resource type: Document, Script, XHR, Image, …
    pub kind: String,
    pub method: String,
    pub status: u16,
    pub mime: String,
    pub ip: String,
    pub port: u16,
    /// `h2`, `h3`, `http/1.1`, …
    pub protocol: String,
    pub tls: Option<Tls>,
    pub headers: Vec<(String, String)>,
    pub bytes: u64,
    pub cached: bool,
    pub failed: Option<String>,
    pub done: bool,
}

#[derive(Default)]
pub struct Inspect {
    pub requests: Vec<Request>,
    by_id: HashMap<String, usize>,
    /// The page's own document (first Document request since the last navigation).
    pub document: Option<usize>,
}

impl Inspect {
    pub(crate) fn reset(&mut self) {
        self.requests.clear();
        self.by_id.clear();
        self.document = None;
    }

    fn entry(&mut self, id: &str) -> Option<&mut Request> {
        self.by_id.get(id).copied().map(|i| &mut self.requests[i])
    }

    fn event(&mut self, method: &str, p: &Value) {
        let id = p["requestId"].as_str().unwrap_or_default();
        match method {
            "Network.requestWillBeSent" => {
                let url = p["request"]["url"].as_str().unwrap_or_default().to_owned();
                if let Some(existing) = self.entry(id) {
                    // Redirect: same request id, new URL.
                    existing.host = host_of(&url);
                    existing.url = url;
                    return;
                }
                if self.requests.len() >= LIMIT {
                    return;
                }
                let kind = p["type"].as_str().unwrap_or("Other").to_owned();
                self.by_id.insert(id.to_owned(), self.requests.len());
                if kind == "Document" && self.document.is_none() {
                    self.document = Some(self.requests.len());
                }
                self.requests.push(Request {
                    host: host_of(&url),
                    url,
                    kind,
                    method: p["request"]["method"].as_str().unwrap_or("GET").to_owned(),
                    ..Default::default()
                });
            }
            "Network.responseReceived" => {
                let r = &p["response"];
                let Some(entry) = self.entry(id) else { return };
                entry.status = r["status"].as_u64().unwrap_or(0) as u16;
                entry.mime = r["mimeType"].as_str().unwrap_or_default().to_owned();
                entry.ip = r["remoteIPAddress"].as_str().unwrap_or_default().trim_matches(['[', ']']).to_owned();
                entry.port = r["remotePort"].as_u64().unwrap_or(0) as u16;
                entry.protocol = r["protocol"].as_str().unwrap_or_default().to_owned();
                entry.cached = r["fromDiskCache"].as_bool().unwrap_or(false) || r["fromServiceWorker"].as_bool().unwrap_or(false);
                if let Some(headers) = r["headers"].as_object() {
                    entry.headers =
                        headers.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_owned())).collect();
                }
                let s = &r["securityDetails"];
                if s.is_object() {
                    entry.tls = Some(Tls {
                        protocol: s["protocol"].as_str().unwrap_or_default().to_owned(),
                        cipher: s["cipher"].as_str().unwrap_or_default().to_owned(),
                        key_exchange: s["keyExchangeGroup"].as_str().or(s["keyExchange"].as_str()).unwrap_or_default().to_owned(),
                        subject: s["subjectName"].as_str().unwrap_or_default().to_owned(),
                        issuer: s["issuer"].as_str().unwrap_or_default().to_owned(),
                        valid_to: s["validTo"].as_f64().unwrap_or(0.0) as i64,
                    });
                }
            }
            "Network.loadingFinished" => {
                if let Some(i) = self.document.filter(|&i| self.by_id.get(id) == Some(&i)) {
                    let d = &self.requests[i];
                    log::debug!(
                        "inspect: document {} → {}:{} {} {:?} headers: {}",
                        d.host,
                        d.ip,
                        d.port,
                        d.protocol,
                        d.tls.as_ref().map(|t| (&t.protocol, &t.issuer)),
                        d.headers.iter().map(|(k, v)| format!("{k}={v}")).filter(|h| h.len() < 60).collect::<Vec<_>>().join(" ")
                    );
                }
                if let Some(entry) = self.entry(id) {
                    entry.bytes = p["encodedDataLength"].as_f64().unwrap_or(0.0) as u64;
                    entry.done = true;
                }
            }
            "Network.loadingFailed" => {
                if let Some(entry) = self.entry(id) {
                    let reason = p["blockedReason"].as_str().or(p["errorText"].as_str()).unwrap_or("failed");
                    entry.failed = Some(reason.to_owned());
                    entry.done = true;
                }
            }
            _ => {}
        }
    }
}

fn host_of(url: &str) -> String {
    url.split_once("://")
        .and_then(|(_, rest)| rest.split(['/', '?', '#']).next())
        .map(|authority| {
            let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
            if host.starts_with('[') { host.to_owned() } else { host.split(':').next().unwrap_or(host).to_owned() }
        })
        .unwrap_or_default()
}

wrap_dev_tools_message_observer! {
    pub(crate) struct Watcher {
        inspect: Rc<RefCell<Inspect>>,
        notify: Rc<dyn Fn()>,
    }

    impl DevToolsMessageObserver {
        fn on_dev_tools_event(&self, _browser: Option<&mut Browser>, method: Option<&CefString>, params: Option<&[u8]>) {
            let (Some(method), Some(params)) = (method, params) else { return };
            let method = method.to_string();
            if !method.starts_with("Network.") {
                return;
            }
            let Ok(params) = serde_json::from_slice::<Value>(params) else { return };
            self.inspect.borrow_mut().event(&method, &params);
            (self.notify)();
        }
    }
}

/// Starts recording `browser`'s network activity. Keep the registration alive.
pub(crate) fn watch(browser: &Browser, inspect: Rc<RefCell<Inspect>>, notify: Rc<dyn Fn()>) -> Option<Registration> {
    let host = browser.host()?;
    let registration = host.add_dev_tools_message_observer(Some(&mut Watcher::new(inspect, notify)));
    // A high id, so the reply can't be mistaken for one of a DevTools frontend's own commands.
    let enable = br#"{"id":2000000001,"method":"Network.enable","params":{"maxPostDataSize":0}}"#;
    host.send_dev_tools_message(Some(enable));
    registration
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn records_requests() {
        let mut i = Inspect::default();
        i.event("Network.requestWillBeSent", &json!({"requestId":"1","type":"Document","request":{"url":"https://example.com/","method":"GET"}}));
        i.event("Network.responseReceived", &json!({"requestId":"1","response":{
            "status":200,"mimeType":"text/html","remoteIPAddress":"[2606:4700::1]","remotePort":443,"protocol":"h2",
            "headers":{"cf-ray":"abc-SOF"},"securityDetails":{"protocol":"TLS 1.3","cipher":"AES_128_GCM","issuer":"E5","subjectName":"example.com","validTo":1800000000.0}}}));
        i.event("Network.loadingFinished", &json!({"requestId":"1","encodedDataLength":1234.0}));
        i.event("Network.requestWillBeSent", &json!({"requestId":"2","type":"Script","request":{"url":"https://cdn.example.net:8443/a.js"}}));
        i.event("Network.loadingFailed", &json!({"requestId":"2","errorText":"net::ERR_ABORTED"}));

        let doc = &i.requests[i.document.unwrap()];
        assert_eq!(doc.ip, "2606:4700::1");
        assert_eq!(doc.protocol, "h2");
        assert_eq!(doc.tls.as_ref().unwrap().protocol, "TLS 1.3");
        assert_eq!(doc.bytes, 1234);
        assert_eq!(i.requests[1].host, "cdn.example.net");
        assert_eq!(i.requests[1].failed.as_deref(), Some("net::ERR_ABORTED"));
    }
}
