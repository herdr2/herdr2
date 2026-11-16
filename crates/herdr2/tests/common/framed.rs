//! Framed-mode test client (`hello {framed: true}`).

use super::Session;
use herdr2::wire;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

pub type Frame = (u32, u8, Vec<u8>);

pub struct Framed {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
    pub stash: VecDeque<Frame>,
}

impl Framed {
    pub fn open(s: &Session) -> Framed {
        s.json(&["server", "start"]);
        let stream = UnixStream::connect(s.socket()).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        writeln!(writer, r#"{{"jsonrpc":"2.0","id":0,"method":"hello","params":{{"framed":true}}}}"#).unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        Framed { reader, writer, next_id: 1, stash: VecDeque::new() }
    }

    pub fn send(&mut self, channel: u32, kind: u8, payload: &[u8]) {
        self.writer.write_all(&wire::encode(channel, kind, payload)).unwrap();
    }

    pub fn read(&mut self, timeout: Duration) -> Option<Frame> {
        self.reader.get_ref().set_read_timeout(Some(timeout)).unwrap();
        let mut header = [0u8; wire::HEADER];
        if self.reader.read_exact(&mut header).is_err() {
            return None;
        }
        self.reader.get_ref().set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let (channel, kind, len) = wire::decode_header(&header).unwrap();
        let mut payload = vec![0; len];
        self.reader.read_exact(&mut payload).unwrap();
        Some((channel, kind, payload))
    }

    pub fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.send(0, wire::DATA, req.to_string().as_bytes());
        loop {
            let frame = self.read(Duration::from_secs(10)).expect("reply");
            if frame.0 == 0 && frame.1 == wire::DATA {
                let reply: Value = serde_json::from_slice(&frame.2).unwrap();
                if reply["id"] == id {
                    return reply;
                }
            }
            self.stash.push_back(frame);
        }
    }
}
