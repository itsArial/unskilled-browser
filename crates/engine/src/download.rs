//! Fetches a URL through Chromium's network stack — same proxy and VPN route as pages — for the
//! few things the browser downloads itself, on request (Web Store packages).

use cef::*;
use std::{cell::RefCell, rc::Rc};

pub type Done = Box<dyn FnOnce(Result<Vec<u8>, String>)>;

thread_local! {
    /// Requests in flight: CEF cancels a request whose object is gone.
    static ACTIVE: RefCell<Vec<Urlrequest>> = const { RefCell::new(Vec::new()) };
}

/// GETs `url` (following redirects) and calls `done` with the body, at most `limit` bytes.
/// UI thread; `done` runs on it too.
pub fn fetch(url: &str, limit: usize, done: Done) {
    let Some(mut request) = request_create() else { return done(Err("couldn't create the request".into())) };
    request.set_url(Some(&CefString::from(url)));
    request.set_method(Some(&CefString::from("GET")));
    let state = Rc::new(Fetch { body: RefCell::new(Vec::new()), done: RefCell::new(Some(done)), limit });
    let mut client = Client::new(state.clone());
    match urlrequest_create(Some(&mut request), Some(&mut client), None) {
        Some(active) => ACTIVE.with_borrow_mut(|a| a.push(active)),
        None => {
            if let Some(done) = state.done.borrow_mut().take() {
                done(Err("couldn't start the download".into()));
            }
        }
    }
}

struct Fetch {
    body: RefCell<Vec<u8>>,
    done: RefCell<Option<Done>>,
    limit: usize,
}

wrap_urlrequest_client! {
    struct Client {
        state: Rc<Fetch>,
    }

    impl UrlrequestClient {
        fn on_download_data(&self, request: Option<&mut Urlrequest>, data: *const u8, data_length: usize) {
            if data.is_null() {
                return;
            }
            let mut body = self.state.body.borrow_mut();
            if body.len() + data_length > self.state.limit {
                if let Some(request) = request {
                    request.cancel();
                }
                return;
            }
            // SAFETY: CEF hands us `data_length` readable bytes for the duration of this call.
            body.extend_from_slice(unsafe { std::slice::from_raw_parts(data, data_length) });
        }

        fn on_request_complete(&self, request: Option<&mut Urlrequest>) {
            let Some(request) = request else { return };
            let status = request.response().map_or(0, |r| r.status());
            let result = if request.request_status() == UrlrequestStatus::SUCCESS && status == 200 {
                Ok(std::mem::take(&mut *self.state.body.borrow_mut()))
            } else if self.state.body.borrow().len() >= self.state.limit {
                Err("the download is too big".into())
            } else if status != 0 {
                Err(format!("the server answered {status}"))
            } else {
                Err("the download failed (offline?)".into())
            };
            let raw = request.get_raw();
            ACTIVE.with_borrow_mut(|a| a.retain(|r| r.get_raw() != raw));
            if let Some(done) = self.state.done.borrow_mut().take() {
                done(result);
            }
        }
    }
}
