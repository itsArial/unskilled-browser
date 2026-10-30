//! Chrome DevTools in our own pane. CEF can't render a DevTools window windowless, so the DevTools
//! frontend loads as an ordinary page and talks to the inspected tab through this bridge: a
//! WebSocket on 127.0.0.1 behind a random 128-bit path, relaying the DevTools protocol in-process.
//! (Chromium's global `--remote-debugging-port` stays off.)

use cef::*;
use std::{
    io::ErrorKind,
    net::TcpListener,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::Duration,
};
use crate::websocket::{self, Message};

pub type Wake = Arc<dyn Fn() + Send + Sync>;

pub(crate) struct Bridge {
    /// What the DevTools frontend page should load.
    pub frontend: String,
    host: BrowserHost,
    inbound: Receiver<String>,
    _registration: Option<Registration>,
    stop: Arc<AtomicBool>,
}

impl Bridge {
    pub fn start(browser: &Browser, wake: Wake) -> Option<Self> {
        let host = browser.host()?;
        let listener = TcpListener::bind("127.0.0.1:0").ok()?;
        listener.set_nonblocking(true).ok()?;
        let port = listener.local_addr().ok()?.port();
        let token = random_token()?;
        let path = format!("/devtools/page/{token}");

        let (inbound_tx, inbound) = mpsc::channel();
        let (outbound_tx, outbound) = mpsc::channel::<Vec<u8>>();
        let registration = host.add_dev_tools_message_observer(Some(&mut Relay::new(outbound_tx)));
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        thread::Builder::new()
            .name("devtools-bridge".into())
            .spawn(move || serve(listener, path, inbound_tx, outbound, wake, stopped))
            .ok()?;

        Some(Self {
            frontend: format!("devtools://devtools/bundled/devtools_app.html?ws=127.0.0.1:{port}/devtools/page/{token}"),
            host,
            inbound,
            _registration: registration,
            stop,
        })
    }

    /// Hands the frontend's commands to Chromium. UI thread, every frame.
    pub fn pump(&self) {
        while let Ok(message) = self.inbound.try_recv() {
            self.host.send_dev_tools_message(Some(message.as_bytes()));
        }
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn random_token() -> Option<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Accepts the frontend (only on the secret path) and relays messages both ways.
fn serve(
    listener: TcpListener,
    path: String,
    inbound: Sender<String>,
    outbound: Receiver<Vec<u8>>,
    wake: Wake,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Relaxed) {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(50));
                continue;
            }
            Err(_) => return,
        };
        let _ = stream.set_nonblocking(false);
        // A client that connects and sends nothing mustn't stall the bridge.
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let Ok(mut socket) = websocket::accept(stream, &path) else {
            log::warn!("devtools: refused a connection on the wrong path");
            continue;
        };
        log::debug!("devtools: frontend connected");
        let _ = socket.stream().set_read_timeout(Some(Duration::from_millis(8)));
        // Drop what queued up while nobody was connected.
        while outbound.try_recv().is_ok() {}
        loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            match socket.read() {
                Ok(Some(Message::Text(text))) => {
                    let _ = inbound.send(text);
                    wake();
                }
                Ok(Some(Message::Close)) | Err(_) => break,
                Ok(None) => {}
            }
            let mut failed = false;
            while let Ok(message) = outbound.try_recv() {
                if socket.send_text(&String::from_utf8_lossy(&message)).is_err() {
                    failed = true;
                    break;
                }
            }
            if failed {
                break;
            }
        }
    }
}

wrap_dev_tools_message_observer! {
    struct Relay {
        out: Sender<Vec<u8>>,
    }

    impl DevToolsMessageObserver {
        fn on_dev_tools_message(&self, _browser: Option<&mut Browser>, message: Option<&[u8]>) -> ::std::os::raw::c_int {
            if let Some(message) = message {
                let _ = self.out.send(message.to_vec());
            }
            1
        }
    }
}
