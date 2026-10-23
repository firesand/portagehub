//! Privileged helper for PortageHub.
//!
//! Started once per session through `pkexec`, it reads JSON requests from
//! stdin and answers with JSON events on stdout. It only runs a fixed list of
//! Portage tools and only writes below `/etc/portage/`.

use portagehub::protocol::{Event, Request};
use portagehub::server::{Server, Sink};
use std::io::{BufRead, Write};
use std::sync::Arc;
use std::sync::mpsc;

fn main() {
    // Single writer thread so events never interleave mid-line.
    let (tx, rx) = mpsc::channel::<Event>();
    let writer = std::thread::spawn(move || {
        let stdout = std::io::stdout();
        for ev in rx {
            let mut lock = stdout.lock();
            if let Ok(s) = serde_json::to_string(&ev) {
                // The GUI may be gone; keep draining so children never block.
                let _ = writeln!(lock, "{s}");
                let _ = lock.flush();
            }
        }
    });

    let uid = unsafe { libc::getuid() };
    let _ = tx.send(Event::Ready { uid });

    let server = Server::default();
    let tx_sink = std::sync::Mutex::new(tx.clone());
    let sink: Sink = Arc::new(move |ev| {
        let _ = tx_sink.lock().unwrap().send(ev);
    });

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Request>(&line) {
            Ok(req) => server.handle(req, sink.clone()),
            Err(e) => {
                let _ = tx.send(Event::Err { id: 0, msg: format!("bad request: {e}") });
            }
        }
    }

    // stdin closed: the GUI exited. Let running jobs finish instead of killing
    // a half-done emerge, then leave.
    while server.running() > 0 {
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    drop(sink);
    drop(tx);
    let _ = writer.join();
}
