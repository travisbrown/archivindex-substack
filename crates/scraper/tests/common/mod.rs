use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

pub fn server(
    responses: Vec<(u16, String, String)>,
) -> (String, std::thread::JoinHandle<Vec<String>>) {
    server_with(|_| responses)
}

pub fn server_with(
    responses: impl FnOnce(&str) -> Vec<(u16, String, String)>,
) -> (String, std::thread::JoinHandle<Vec<String>>) {
    server_with_connection(responses, |_| {})
}

pub fn server_with_connection(
    responses: impl FnOnce(&str) -> Vec<(u16, String, String)>,
    mut connect: impl FnMut(&mut TcpStream) + Send + 'static,
) -> (String, std::thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let responses = responses(&base);
    let handle = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for (status, headers, body) in responses {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "timed out waiting for request");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("accept: {e}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            connect(&mut stream);
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            let request = String::from_utf8(request).unwrap();
            let head = request.starts_with("HEAD ");
            requests.push(request);
            write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n", body.len()).unwrap();
            if !head {
                stream.write_all(body.as_bytes()).unwrap();
            }
        }
        requests
    });
    (base, handle)
}
