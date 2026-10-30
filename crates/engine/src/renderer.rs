//! Runs inside Chromium's renderer processes (sandboxed, no disk or network of their own).
//!
//! At V8 context creation — before any page script — we fetch the shield's page script
//! synchronously over our own CSP-bypassing scheme, evaluate it natively (not subject to the
//! page's CSP or Trusted Types) and hand it a native `query` function that never touches `window`.

use crate::net::SHIELD_SCHEME;
use cef::*;
use std::{cell::RefCell, collections::HashMap};

pub const MSG_QUERY: &str = "shield-query";
pub const MSG_REPLY: &str = "shield-reply";
/// Internal page → browser: `[id, method, json]`, answered with `MSG_REPLY`.
pub const MSG_WEBUI: &str = "webui-call";
/// Page → browser: `[kind, json]`, kind `submit` (a login was submitted) or `form` (one is shown).
pub const MSG_LOGIN: &str = "login";
/// Browser → page: `[username, password]`, fill the login form.
pub const MSG_FILL: &str = "login-fill";

const FORMS_SCRIPT: &str = include_str!("../assets/forms.js");
const BRIDGE_SCRIPT: &str = "(function (send) {\
    Object.defineProperty(window, 'unskilled', { value: Object.freeze({\
        call(method, params) {\
            return new Promise((resolve) => send(method, JSON.stringify(params), (json) => resolve(JSON.parse(json))));\
        },\
    }) });\
})";

thread_local! {
    /// Pending generic-cosmetic callbacks: id → (context, callback).
    static PENDING: RefCell<HashMap<i32, (V8Context, V8Value)>> = RefCell::new(HashMap::new());
    static NEXT_ID: RefCell<i32> = const { RefCell::new(1) };
    /// Per frame: the login-form script's `fill` function.
    static FILLERS: RefCell<HashMap<String, (V8Context, V8Value)>> = RefCell::new(HashMap::new());
}

fn next_id() -> i32 {
    NEXT_ID.with_borrow_mut(|n| {
        *n += 1;
        *n
    })
}

wrap_render_process_handler! {
    pub struct Renderer;

    impl RenderProcessHandler {
        fn on_context_created(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            context: Option<&mut V8Context>,
        ) {
            let (Some(frame), Some(context)) = (frame, context) else { return };
            let url = CefString::from(&frame.url()).to_string();
            if url.starts_with("http://") || url.starts_with("https://") {
                inject(context);
                watch_forms(frame, context);
            } else if url.starts_with("unskilled://") {
                // The bridge to browser data exists only on our own pages (and the browser
                // re-checks the sender on every call).
                if let Some(bridge) = eval(context, BRIDGE_SCRIPT).filter(|v| v.is_function() != 0) {
                    let send = v8_value_create_function(Some(&CefString::from("send")), Some(&mut BridgeHandler::new()));
                    bridge.execute_function_with_context(Some(context), None, Some(&[send]));
                }
            }
        }

        fn on_context_released(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            context: Option<&mut V8Context>,
        ) {
            let Some(context) = context else { return };
            PENDING.with_borrow_mut(|p| p.retain(|_, (ctx, _)| ctx.is_same(Some(context)) == 0));
            FILLERS.with_borrow_mut(|f| f.retain(|_, (ctx, _)| ctx.is_same(Some(context)) == 0));
        }

        fn on_process_message_received(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> ::std::os::raw::c_int {
            let Some(message) = message else { return 0 };
            let name = CefString::from(&message.name()).to_string();
            if name == MSG_FILL {
                let (Some(frame), Some(args)) = (_frame, message.argument_list()) else { return 1 };
                let id = CefString::from(&frame.identifier()).to_string();
                if let Some((mut context, fill)) = FILLERS.with_borrow(|f| f.get(&id).cloned())
                    && context.is_valid() != 0 {
                        let user = v8_value_create_string(Some(&CefString::from(&args.string(0))));
                        let pass = v8_value_create_string(Some(&CefString::from(&args.string(1))));
                        fill.execute_function_with_context(Some(&mut context), None, Some(&[user, pass]));
                    }
                return 1;
            }
            if name != MSG_REPLY {
                return 0;
            }
            let Some(args) = message.argument_list() else { return 1 };
            let id = args.int(0);
            let css = args.string(1);
            if let Some((mut context, callback)) = PENDING.with_borrow_mut(|p| p.remove(&id))
                && context.is_valid() != 0 {
                    let arg = v8_value_create_string(Some(&CefString::from(&css)));
                    callback.execute_function_with_context(Some(&mut context), None, Some(&[arg]));
                }
            1
        }
    }
}

fn inject(context: &mut V8Context) {
    // Synchronous XHR from the page context: blocks this renderer briefly while the browser
    // process answers, which guarantees scriptlets run before the page's own scripts.
    let fetch = format!(
        "(function () {{ try {{ var x = new XMLHttpRequest(); \
         x.open('GET', '{SHIELD_SCHEME}://inject/?u=' + encodeURIComponent(location.href), false); \
         x.send(); return x.status === 200 ? x.responseText : ''; }} catch (e) {{ return ''; }} }})()"
    );
    let Some(source) = eval(context, &fetch).filter(|v| v.is_string() != 0) else { return };
    let source = CefString::from(&source.string_value()).to_string();
    if source.is_empty() {
        return;
    }
    let Some(function) = eval(context, &source).filter(|v| v.is_function() != 0) else {
        log::warn!("shield: page script did not evaluate to a function");
        return;
    };
    let query = v8_value_create_function(Some(&CefString::from("query")), Some(&mut QueryHandler::new()));
    function.execute_function_with_context(Some(context), None, Some(&[query]));
}

/// Installs the login-form watcher in a frame and keeps its `fill` function for later.
fn watch_forms(frame: &mut Frame, context: &mut V8Context) {
    let Some(script) = eval(context, FORMS_SCRIPT).filter(|v| v.is_function() != 0) else { return };
    let report = v8_value_create_function(Some(&CefString::from("report")), Some(&mut LoginHandler::new()));
    let Some(fill) = script.execute_function_with_context(Some(context), None, Some(&[report])) else { return };
    if fill.is_function() != 0 {
        let id = CefString::from(&frame.identifier()).to_string();
        FILLERS.with_borrow_mut(|f| f.insert(id, (context.clone(), fill)));
    }
}

fn send_to_browser(name: &str, args: &[&str]) {
    let Some(frame) = v8_context_get_current_context().and_then(|c| c.frame()) else { return };
    let Some(mut message) = process_message_create(Some(&CefString::from(name))) else { return };
    if let Some(list) = message.argument_list() {
        for (i, arg) in args.iter().enumerate() {
            list.set_string(i, Some(&CefString::from(*arg)));
        }
    }
    frame.send_process_message(ProcessId::BROWSER, Some(&mut message));
}

fn string_arg(args: &[Option<V8Value>], i: usize) -> Option<String> {
    args.get(i)?.as_ref().filter(|v| v.is_string() != 0).map(|v| CefString::from(&v.string_value()).to_string())
}

wrap_v8_handler! {
    struct LoginHandler;

    impl V8Handler {
        fn execute(
            &self,
            _name: Option<&CefString>,
            _object: Option<&mut V8Value>,
            arguments: Option<&[Option<V8Value>]>,
            _retval: Option<&mut Option<V8Value>>,
            _exception: Option<&mut CefString>,
        ) -> ::std::os::raw::c_int {
            let Some(args) = arguments else { return 1 };
            if let (Some(kind), Some(json)) = (string_arg(args, 0), string_arg(args, 1)) {
                send_to_browser(MSG_LOGIN, &[&kind, &json]);
            }
            1
        }
    }
}

wrap_v8_handler! {
    struct BridgeHandler;

    impl V8Handler {
        fn execute(
            &self,
            _name: Option<&CefString>,
            _object: Option<&mut V8Value>,
            arguments: Option<&[Option<V8Value>]>,
            _retval: Option<&mut Option<V8Value>>,
            _exception: Option<&mut CefString>,
        ) -> ::std::os::raw::c_int {
            let Some(args) = arguments.filter(|a| a.len() == 3) else { return 1 };
            let (Some(method), Some(json)) = (string_arg(args, 0), string_arg(args, 1)) else { return 1 };
            let Some(callback) = args[2].clone().filter(|c| c.is_function() != 0) else { return 1 };
            let Some(context) = v8_context_get_current_context() else { return 1 };
            let id = next_id();
            PENDING.with_borrow_mut(|p| p.insert(id, (context, callback)));
            send_to_browser(MSG_WEBUI, &[&id.to_string(), &method, &json]);
            1
        }
    }
}

fn eval(context: &mut V8Context, code: &str) -> Option<V8Value> {
    let mut retval = None;
    let mut exception = None;
    let ok = context.eval(Some(&CefString::from(code)), Some(&CefString::from("")), 0, Some(&mut retval), Some(&mut exception));
    if ok == 0 { None } else { retval }
}

fn string_array(value: Option<&V8Value>) -> Vec<String> {
    let Some(value) = value.filter(|v| v.is_array() != 0) else { return vec![] };
    (0..value.array_length())
        .filter_map(|i| value.value_byindex(i))
        .filter(|v| v.is_string() != 0)
        .map(|v| CefString::from(&v.string_value()).to_string())
        .collect()
}

fn json_array(items: &[String]) -> String {
    let mut out = String::from("[");
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        for c in item.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('"');
    }
    out.push(']');
    out
}

wrap_v8_handler! {
    struct QueryHandler;

    impl V8Handler {
        fn execute(
            &self,
            _name: Option<&CefString>,
            _object: Option<&mut V8Value>,
            arguments: Option<&[Option<V8Value>]>,
            _retval: Option<&mut Option<V8Value>>,
            _exception: Option<&mut CefString>,
        ) -> ::std::os::raw::c_int {
            let Some(args) = arguments.filter(|a| a.len() == 3) else { return 1 };
            let Some(callback) = args[2].clone().filter(|c| c.is_function() != 0) else { return 1 };
            let Some(context) = v8_context_get_current_context() else { return 1 };
            let Some(frame) = context.frame() else { return 1 };

            let id = next_id();
            let classes = string_array(args[0].as_ref());
            let ids = string_array(args[1].as_ref());
            let Some(mut message) = process_message_create(Some(&CefString::from(MSG_QUERY))) else { return 1 };
            if let Some(list) = message.argument_list() {
                list.set_int(0, id);
                list.set_string(1, Some(&CefString::from(&frame.url())));
                list.set_string(2, Some(&CefString::from(json_array(&classes).as_str())));
                list.set_string(3, Some(&CefString::from(json_array(&ids).as_str())));
            }
            PENDING.with_borrow_mut(|p| p.insert(id, (context, callback)));
            frame.send_process_message(ProcessId::BROWSER, Some(&mut message));
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::json_array;

    #[test]
    fn escapes_json() {
        assert_eq!(json_array(&["a\"b".into(), "c\\d".into()]), r#"["a\"b","c\\d"]"#);
    }
}
