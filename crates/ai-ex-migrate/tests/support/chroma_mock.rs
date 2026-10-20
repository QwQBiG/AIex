use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub body: Option<Value>,
}

pub struct Step {
    request: Request,
    status: u16,
    response: Vec<u8>,
    extra_content_length: usize,
}

impl Step {
    pub fn json(method: &str, path: &str, body: Option<Value>, response: Value) -> Self {
        Self {
            request: Request {
                method: method.to_owned(),
                path: path.to_owned(),
                body,
            },
            status: 200,
            response: serde_json::to_vec(&response).unwrap(),
            extra_content_length: 0,
        }
    }

    pub fn status(mut self, status: u16) -> Self {
        self.status = status;
        self
    }

    pub fn truncated(mut self) -> Self {
        self.extra_content_length = 17;
        self
    }
}

#[derive(Default)]
struct State {
    requests: Vec<Request>,
    errors: Vec<String>,
    remaining: usize,
}

pub struct Mock {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    state: Arc<Mutex<State>>,
    worker: Option<JoinHandle<()>>,
}

impl Mock {
    pub fn start(steps: Vec<Step>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(State {
            remaining: steps.len(),
            ..State::default()
        }));
        let server_stop = Arc::clone(&stop);
        let server_state = Arc::clone(&state);
        let worker = thread::spawn(move || {
            let mut steps: VecDeque<_> = steps.into();
            let deadline = Instant::now() + Duration::from_secs(30);
            while !server_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // Windows can inherit the listener's nonblocking setting.
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(5)))
                            .unwrap();
                        stream
                            .set_write_timeout(Some(Duration::from_secs(5)))
                            .unwrap();
                        let request = match read_request(&mut stream) {
                            Ok(request) => request,
                            Err(error) => {
                                server_state.lock().unwrap().errors.push(error);
                                break;
                            }
                        };
                        let mut state = server_state.lock().unwrap();
                        state.requests.push(request.clone());
                        let Some(step) = steps.pop_front() else {
                            state
                                .errors
                                .push(format!("unexpected request: {request:?}"));
                            drop(state);
                            let _ = stream.write_all(b"HTTP/1.1 500 Unexpected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                            continue;
                        };
                        state.remaining = steps.len();
                        if request != step.request {
                            state
                                .errors
                                .push(format!("expected {:?}, received {request:?}", step.request));
                            drop(state);
                            let _ = stream.write_all(b"HTTP/1.1 500 Unexpected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                            continue;
                        }
                        drop(state);
                        let header = format!(
                            "HTTP/1.1 {} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            step.status,
                            step.response.len() + step.extra_content_length
                        );
                        if let Err(error) = stream
                            .write_all(header.as_bytes())
                            .and_then(|()| stream.write_all(&step.response))
                        {
                            server_state.lock().unwrap().errors.push(error.to_string());
                            break;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            server_state
                                .lock()
                                .unwrap()
                                .errors
                                .push("mock deadline expired".to_owned());
                            break;
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => {
                        server_state.lock().unwrap().errors.push(error.to_string());
                        break;
                    }
                }
            }
        });
        Self {
            address,
            stop,
            state,
            worker: Some(worker),
        }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    pub fn finish(mut self) -> Vec<Request> {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap();
        let state = self.state.lock().unwrap();
        assert!(state.errors.is_empty(), "mock errors: {:?}", state.errors);
        assert_eq!(state.remaining, 0, "mock requests: {:?}", state.requests);
        state.requests.clone()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Result<Request, String> {
    let mut bytes = Vec::new();
    let header_end = loop {
        if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break index + 4;
        }
        read_more(stream, &mut bytes)?;
    };
    let header = std::str::from_utf8(&bytes[..header_end]).map_err(|e| e.to_string())?;
    let mut lines = header.split("\r\n");
    let mut first = lines
        .next()
        .ok_or("missing request line")?
        .split_whitespace();
    let method = first.next().ok_or("missing HTTP method")?.to_owned();
    let path = first.next().ok_or("missing HTTP path")?.to_owned();
    let mut content_length = 0;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or("invalid HTTP header")?;
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.trim().parse::<usize>().map_err(|e| e.to_string())?;
        }
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err("mock expects Content-Length, not a transfer encoding".to_owned());
        }
    }
    if content_length > 1024 * 1024 {
        return Err("mock request body exceeds 1 MiB".to_owned());
    }
    while bytes.len() < header_end + content_length {
        read_more(stream, &mut bytes)?;
    }
    let body = if content_length == 0 {
        None
    } else {
        Some(
            serde_json::from_slice(&bytes[header_end..header_end + content_length])
                .map_err(|e| e.to_string())?,
        )
    };
    Ok(Request { method, path, body })
}

fn read_more(stream: &mut TcpStream, bytes: &mut Vec<u8>) -> Result<(), String> {
    let mut buffer = [0; 4096];
    let read = stream.read(&mut buffer).map_err(|e| e.to_string())?;
    if read == 0 {
        return Err("HTTP request ended early".to_owned());
    }
    bytes.extend_from_slice(&buffer[..read]);
    if bytes.len() > 1024 * 1024 + 64 * 1024 {
        return Err("mock request exceeds its byte budget".to_owned());
    }
    Ok(())
}
