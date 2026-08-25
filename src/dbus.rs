//! Just enough D-Bus to own a name and export an object.
//!
//! The tray icon needs to speak StatusNotifierItem, which is D-Bus, and the
//! rest of busywatch has no dependencies — so this is the protocol by hand,
//! the same way `web.rs` is HTTP by hand. It implements only what the tray
//! uses: a session-bus connection over a unix socket, SASL EXTERNAL auth,
//! marshalling for the handful of types SNI properties are made of, and a
//! blocking read loop.
//!
//! Everything is little-endian ('l'), which is what we emit; incoming
//! messages from a big-endian peer would need byte-swapping and are not
//! handled, because no such peer exists on a machine we share a socket with.

use std::env;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

// ------------------------------------------------------------------ encoding

/// A D-Bus marshalling buffer. Alignment is relative to the start of the
/// buffer, which is why a body is always built in its own `Enc`: in the wire
/// format a body's padding counts from the body's own start, not the
/// message's.
#[derive(Default)]
pub struct Enc {
    pub buf: Vec<u8>,
}

impl Enc {
    pub fn new() -> Enc {
        Enc::default()
    }

    pub fn align(&mut self, n: usize) {
        while self.buf.len() % n != 0 {
            self.buf.push(0);
        }
    }

    pub fn byte(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub fn u32(&mut self, v: u32) {
        self.align(4);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn i32(&mut self, v: i32) {
        self.align(4);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn boolean(&mut self, v: bool) {
        self.u32(v as u32);
    }

    /// STRING and OBJECT_PATH share a representation: u32 length, bytes, NUL.
    pub fn string(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
    }

    /// SIGNATURE is length-prefixed by a single byte and needs no alignment.
    pub fn signature(&mut self, s: &str) {
        self.buf.push(s.len() as u8);
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
    }

    /// A VARIANT: its signature, then the value written by `f`.
    pub fn variant(&mut self, sig: &str, f: impl FnOnce(&mut Enc)) {
        self.signature(sig);
        f(self);
    }

    pub fn variant_str(&mut self, sig: &str, v: &str) {
        self.variant(sig, |e| e.string(v));
    }

    /// An ARRAY: a u32 byte-count of the contents, then the contents, whose
    /// first element is aligned to `elem_align` *after* the count.
    pub fn array(&mut self, elem_align: usize, f: impl FnOnce(&mut Enc)) {
        self.align(4);
        let len_at = self.buf.len();
        self.buf.extend_from_slice(&0u32.to_le_bytes());
        self.align(elem_align);
        let start = self.buf.len();
        f(self);
        let len = (self.buf.len() - start) as u32;
        self.buf[len_at..len_at + 4].copy_from_slice(&len.to_le_bytes());
    }

    /// A STRUCT: 8-aligned, contents written by `f`.
    pub fn strukt(&mut self, f: impl FnOnce(&mut Enc)) {
        self.align(8);
        f(self);
    }

    /// One `a{sv}` entry.
    pub fn dict_str(&mut self, key: &str, sig: &str, f: impl FnOnce(&mut Enc)) {
        self.align(8);
        self.string(key);
        self.variant(sig, f);
    }
}

// ------------------------------------------------------------------ decoding

pub struct Dec<'a> {
    pub buf: &'a [u8],
    pub pos: usize,
}

impl<'a> Dec<'a> {
    pub fn new(buf: &'a [u8]) -> Dec<'a> {
        Dec { buf, pos: 0 }
    }

    fn align(&mut self, n: usize) {
        while self.pos % n != 0 {
            self.pos += 1;
        }
    }

    pub fn byte(&mut self) -> Option<u8> {
        let v = *self.buf.get(self.pos)?;
        self.pos += 1;
        Some(v)
    }

    pub fn u32(&mut self) -> Option<u32> {
        self.align(4);
        let b = self.buf.get(self.pos..self.pos + 4)?;
        self.pos += 4;
        Some(u32::from_le_bytes(b.try_into().ok()?))
    }

    pub fn i32(&mut self) -> Option<i32> {
        self.u32().map(|v| v as i32)
    }

    pub fn string(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        let s = self.buf.get(self.pos..self.pos + n)?;
        self.pos += n + 1; // trailing NUL
        Some(String::from_utf8_lossy(s).into_owned())
    }

    pub fn signature(&mut self) -> Option<String> {
        let n = self.byte()? as usize;
        let s = self.buf.get(self.pos..self.pos + n)?;
        self.pos += n + 1;
        Some(String::from_utf8_lossy(s).into_owned())
    }
}

// ------------------------------------------------------------------ messages

pub const METHOD_CALL: u8 = 1;
pub const METHOD_RETURN: u8 = 2;
pub const ERROR: u8 = 3;
pub const SIGNAL: u8 = 4;

const NO_REPLY_EXPECTED: u8 = 1;

#[derive(Default, Debug, Clone)]
pub struct Msg {
    pub kind: u8,
    pub flags: u8,
    pub serial: u32,
    pub path: Option<String>,
    pub iface: Option<String>,
    pub member: Option<String>,
    pub err_name: Option<String>,
    pub reply_serial: Option<u32>,
    pub dest: Option<String>,
    pub sender: Option<String>,
    pub sig: Option<String>,
    pub body: Vec<u8>,
}

impl Msg {
    pub fn call(dest: &str, path: &str, iface: &str, member: &str) -> Msg {
        Msg {
            kind: METHOD_CALL,
            dest: Some(dest.into()),
            path: Some(path.into()),
            iface: Some(iface.into()),
            member: Some(member.into()),
            ..Msg::default()
        }
    }

    pub fn signal(path: &str, iface: &str, member: &str) -> Msg {
        Msg {
            kind: SIGNAL,
            flags: NO_REPLY_EXPECTED,
            path: Some(path.into()),
            iface: Some(iface.into()),
            member: Some(member.into()),
            ..Msg::default()
        }
    }

    /// A METHOD_RETURN answering `to`.
    pub fn reply_to(to: &Msg) -> Msg {
        Msg {
            kind: METHOD_RETURN,
            flags: NO_REPLY_EXPECTED,
            reply_serial: Some(to.serial),
            dest: to.sender.clone(),
            ..Msg::default()
        }
    }

    pub fn error_to(to: &Msg, name: &str, text: &str) -> Msg {
        let mut e = Enc::new();
        e.string(text);
        Msg {
            kind: ERROR,
            flags: NO_REPLY_EXPECTED,
            reply_serial: Some(to.serial),
            dest: to.sender.clone(),
            err_name: Some(name.into()),
            sig: Some("s".into()),
            body: e.buf,
            ..Msg::default()
        }
    }

    pub fn with_body(mut self, sig: &str, body: Vec<u8>) -> Msg {
        self.sig = Some(sig.into());
        self.body = body;
        self
    }

    fn encode(&self, serial: u32) -> Vec<u8> {
        let mut e = Enc::new();
        e.byte(b'l');
        e.byte(self.kind);
        e.byte(self.flags);
        e.byte(1); // protocol version
        e.u32(self.body.len() as u32);
        e.u32(serial);

        // The header field array. Its length is only known once written, so
        // the slot is reserved and patched — same trick as Enc::array, but
        // spelled out because the elements are heterogeneous.
        e.align(4);
        let len_at = e.buf.len();
        e.buf.extend_from_slice(&0u32.to_le_bytes());
        e.align(8);
        let start = e.buf.len();
        let mut field = |e: &mut Enc, code: u8, sig: &str, v: &str| {
            e.align(8);
            e.byte(code);
            e.variant_str(sig, v);
        };
        if let Some(v) = &self.path {
            field(&mut e, 1, "o", v);
        }
        if let Some(v) = &self.iface {
            field(&mut e, 2, "s", v);
        }
        if let Some(v) = &self.member {
            field(&mut e, 3, "s", v);
        }
        if let Some(v) = &self.err_name {
            field(&mut e, 4, "s", v);
        }
        if let Some(v) = self.reply_serial {
            e.align(8);
            e.byte(5);
            e.variant("u", |e| e.u32(v));
        }
        if let Some(v) = &self.dest {
            field(&mut e, 6, "s", v);
        }
        if let Some(v) = &self.sender {
            field(&mut e, 7, "s", v);
        }
        if let Some(v) = &self.sig {
            e.align(8);
            e.byte(8);
            e.variant("g", |e| e.signature(v));
        }
        let arr_len = (e.buf.len() - start) as u32;
        e.buf[len_at..len_at + 4].copy_from_slice(&arr_len.to_le_bytes());

        // The body always starts on an 8-byte boundary.
        e.align(8);
        e.buf.extend_from_slice(&self.body);
        e.buf
    }

    fn decode(head: &[u8], rest: &[u8]) -> Option<Msg> {
        let mut m = Msg { kind: head[1], flags: head[2], ..Msg::default() };
        m.serial = u32::from_le_bytes(head[8..12].try_into().ok()?);
        let arr_len = u32::from_le_bytes(head[12..16].try_into().ok()?) as usize;
        let mut d = Dec::new(&rest[..arr_len.min(rest.len())]);
        while d.pos < d.buf.len() {
            d.align(8);
            let Some(code) = d.byte() else { break };
            let Some(sig) = d.signature() else { break };
            match (code, sig.as_str()) {
                (1, "o") => m.path = d.string(),
                (2, "s") => m.iface = d.string(),
                (3, "s") => m.member = d.string(),
                (4, "s") => m.err_name = d.string(),
                (5, "u") => m.reply_serial = d.u32(),
                (6, "s") => m.dest = d.string(),
                (7, "s") => m.sender = d.string(),
                (8, "g") => m.sig = d.signature(),
                // An unknown field still has to be stepped over, and the only
                // types the bus sends here are these; anything else means the
                // stream is out of sync, so stop rather than guess.
                _ => break,
            }
        }
        let body_at = {
            let mut p = 16 + arr_len;
            while p % 8 != 0 {
                p += 1;
            }
            p - 16
        };
        m.body = rest.get(body_at..).unwrap_or(&[]).to_vec();
        Some(m)
    }
}

// --------------------------------------------------------------- connection

/// The write half, behind its own lock. The tray thread parks in a blocking
/// `read()` while the watch loop emits property-change signals from another
/// thread, so writes are serialised here and the serial counter lives with
/// them — two threads handing out the same serial would confuse every reply.
pub struct Tx {
    sock: UnixStream,
    serial: u32,
}

impl Tx {
    pub fn send(&mut self, m: Msg) -> io::Result<u32> {
        self.serial += 1;
        let serial = self.serial;
        self.sock.write_all(&m.encode(serial))?;
        self.sock.flush()?;
        Ok(serial)
    }
}

pub struct Conn {
    rx: UnixStream,
    tx: Arc<Mutex<Tx>>,
    pub unique: String,
}

impl Conn {
    /// Connect to the session bus and complete auth. Returns None when there
    /// is no session bus at all — a headless run, or a service started
    /// outside a user session — which is not an error, just no tray.
    pub fn session() -> Option<Conn> {
        let addr = env::var("DBUS_SESSION_BUS_ADDRESS").ok()?;
        // unix:path=/run/user/1000/bus, sometimes with extra ,guid=… parts.
        let path = addr
            .split(',')
            .find_map(|p| p.trim().strip_prefix("unix:path=").or_else(|| p.trim().strip_prefix("path=")))?;
        let mut sock = UnixStream::connect(path).ok()?;

        // SASL EXTERNAL: the leading NUL, then our uid in hex as the identity.
        let uid = unsafe { libc::getuid() };
        let uid_hex: String = uid.to_string().bytes().map(|b| format!("{b:02x}")).collect();
        sock.write_all(&[0]).ok()?;
        sock.write_all(format!("AUTH EXTERNAL {uid_hex}\r\n").as_bytes()).ok()?;
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            sock.read_exact(&mut byte).ok()?;
            line.push(byte[0]);
            if line.ends_with(b"\r\n") {
                break;
            }
        }
        if !line.starts_with(b"OK") {
            return None;
        }
        sock.write_all(b"BEGIN\r\n").ok()?;

        let rx = sock.try_clone().ok()?;
        let mut c = Conn {
            rx,
            tx: Arc::new(Mutex::new(Tx { sock, serial: 0 })),
            unique: String::new(),
        };
        let hello = Msg::call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "Hello",
        );
        let reply = c.call_sync(hello).ok()?;
        c.unique = Dec::new(&reply.body).string()?;
        Some(c)
    }

    /// A handle other threads can send on while this one blocks in `read`.
    pub fn sender(&self) -> Arc<Mutex<Tx>> {
        Arc::clone(&self.tx)
    }

    pub fn send(&mut self, m: Msg) -> io::Result<u32> {
        self.tx.lock().map_err(|_| io::Error::other("bus lock poisoned"))?.send(m)
    }

    pub fn read(&mut self) -> io::Result<Msg> {
        let mut head = [0u8; 16];
        self.rx.read_exact(&mut head)?;
        if head[0] != b'l' {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "big-endian D-Bus peer"));
        }
        let body_len = u32::from_le_bytes(head[4..8].try_into().unwrap()) as usize;
        let arr_len = u32::from_le_bytes(head[12..16].try_into().unwrap()) as usize;
        let mut padded = arr_len;
        while (16 + padded) % 8 != 0 {
            padded += 1;
        }
        let mut rest = vec![0u8; padded + body_len];
        self.rx.read_exact(&mut rest)?;
        Msg::decode(&head, &rest)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed D-Bus message"))
    }

    /// Send and read until the matching reply arrives. Messages that arrive
    /// in the meantime are dropped: this is only used during setup, before
    /// anyone can be calling us.
    pub fn call_sync(&mut self, m: Msg) -> io::Result<Msg> {
        let serial = self.send(m)?;
        loop {
            let r = self.read()?;
            if r.reply_serial == Some(serial) {
                if r.kind == ERROR {
                    let text = Dec::new(&r.body).string().unwrap_or_default();
                    let name = r.err_name.clone().unwrap_or_default();
                    return Err(io::Error::other(format!("{name}: {text}")));
                }
                return Ok(r);
            }
        }
    }

    pub fn request_name(&mut self, name: &str) -> io::Result<()> {
        let mut e = Enc::new();
        e.string(name);
        e.u32(0);
        self.call_sync(
            Msg::call(
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "RequestName",
            )
            .with_body("su", e.buf),
        )?;
        Ok(())
    }
}

#[cfg(test)]
pub fn encode_for_test(m: &Msg, serial: u32) -> Vec<u8> {
    m.encode(serial)
}

#[cfg(test)]
pub fn decode_for_test(wire: &[u8]) -> Option<Msg> {
    Msg::decode(&wire[..16], &wire[16..])
}
