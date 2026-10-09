// Discord Rich Presence ("Listening to …") over Discord's local IPC socket.
//
// One worker thread owns the socket: it connects when there is something to show (the
// Discord app, Flatpak / Snap Discord, or Vesktop / arRPC), retries every 15 s while Discord
// is closed, and sends the latest activity. Frames are `op: u32 LE, len: u32 LE, JSON`.
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    sync::mpsc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};

use crate::{model::Item, settings};

/// Application id baked into release builds (APPLEJACK_DISCORD_CLIENT_ID at build time);
/// Preferences can set another.
pub fn client_id() -> String {
    settings::with(|s| {
        if s.discord_client_id.trim().is_empty() {
            option_env!("APPLEJACK_DISCORD_CLIENT_ID").unwrap_or("").to_owned()
        } else {
            s.discord_client_id.trim().to_owned()
        }
    })
}

#[derive(Clone, PartialEq)]
pub struct Presence {
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub art: Option<String>,
    pub url: Option<String>,
    /// Unix ms when the track started / will end (playing only).
    pub span: Option<(u64, u64)>,
}

enum Msg {
    Set(Option<Presence>, String),
}

thread_local! {
    static TX: mpsc::Sender<Msg> = spawn();
    static LAST: std::cell::RefCell<(Option<Presence>, String)> = const { std::cell::RefCell::new((None, String::new())) };
}

/// What plays now; None clears the status. Repeated identical calls cost nothing.
pub fn update(it: Option<&Item>, playing: bool, position: f64, duration: f64) {
    let enabled = settings::with(|s| s.discord);
    let p = it.filter(|_| enabled && playing).map(|it| {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        let start = now.saturating_sub((position.max(0.0) * 1000.0) as u64);
        Presence {
            title: it.title().to_owned(),
            artist: it.artist().to_owned(),
            album: it.album.clone().filter(|a| !a.is_empty()),
            art: it.art(512),
            url: it.catalog_song_id().map(|id| format!("https://music.apple.com/song/{id}")),
            span: (duration > 0.0).then(|| (start, start + (duration * 1000.0) as u64)),
        }
    });
    let id = client_id();
    let same = LAST.with(|l| {
        let l = l.borrow();
        if l.1 != id {
            return false;
        }
        match (&l.0, &p) {
            (None, None) => true,
            // Position drifts a little between calls: only a jump of 2 s counts.
            (Some(a), Some(b)) => {
                a.title == b.title
                    && a.artist == b.artist
                    && a.art == b.art
                    && match (a.span, b.span) {
                        (Some((x, _)), Some((y, _))) => x.abs_diff(y) < 2000,
                        (None, None) => true,
                        _ => false,
                    }
            }
            _ => false,
        }
    });
    if same {
        return;
    }
    LAST.with(|l| *l.borrow_mut() = (p.clone(), id.clone()));
    TX.with(|tx| {
        let _ = tx.send(Msg::Set(p, id));
    });
}

fn sockets() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for var in ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"] {
        if let Some(d) = std::env::var_os(var) {
            dirs.push(d.into());
        }
    }
    dirs.push("/tmp".into());
    let mut out = Vec::new();
    for d in dirs {
        for sub in ["", "app/com.discordapp.Discord", "app/com.discordapp.DiscordCanary", "app/dev.vencord.Vesktop", "snap.discord", ".flatpak/dev.vencord.Vesktop/xdg-run"] {
            for n in 0..10 {
                out.push(d.join(sub).join(format!("discord-ipc-{n}")));
            }
        }
    }
    out
}

struct Conn {
    stream: UnixStream,
    client: String,
}

fn frame(stream: &mut UnixStream, op: u32, body: &Value) -> std::io::Result<()> {
    let data = body.to_string();
    let mut buf = Vec::with_capacity(8 + data.len());
    buf.extend_from_slice(&op.to_le_bytes());
    buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
    buf.extend_from_slice(data.as_bytes());
    stream.write_all(&buf)
}

fn read_frame(stream: &mut UnixStream) -> std::io::Result<(u32, Value)> {
    let mut head = [0u8; 8];
    stream.read_exact(&mut head)?;
    let op = u32::from_le_bytes(head[..4].try_into().unwrap());
    let len = u32::from_le_bytes(head[4..].try_into().unwrap()) as usize;
    if len > 1 << 20 {
        return Err(std::io::Error::other("frame too large"));
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body)?;
    Ok((op, serde_json::from_slice(&body).unwrap_or(Value::Null)))
}

fn connect(client: &str) -> Option<Conn> {
    for path in sockets() {
        let Ok(mut stream) = UnixStream::connect(&path) else { continue };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
        if frame(&mut stream, 0, &json!({ "v": 1, "client_id": client })).is_err() {
            continue;
        }
        match read_frame(&mut stream) {
            Ok((1, v)) if v.get("evt").and_then(Value::as_str) == Some("READY") => {
                return Some(Conn { stream, client: client.to_owned() });
            }
            Ok((_, v)) => eprintln!("discord: handshake refused: {v}"),
            Err(e) => eprintln!("discord: {e}"),
        }
    }
    None
}

fn activity(p: &Presence, display_type: bool) -> Value {
    // Discord wants 2..128 characters in these fields.
    let fit = |s: &str| {
        let s: String = s.chars().take(128).collect();
        if s.chars().count() < 2 { format!("{s}  ") } else { s }
    };
    let mut a = json!({
        "type": 2, // Listening
        "details": fit(&p.title),
        "state": fit(&p.artist),
    });
    if display_type {
        a["status_display_type"] = json!(1); // the member list says "Listening to <artist>"
    }
    let mut assets = json!({});
    if let Some(art) = &p.art {
        assets["large_image"] = json!(art);
    }
    if let Some(album) = &p.album {
        assets["large_text"] = json!(fit(album));
    }
    a["assets"] = assets;
    if let Some((start, end)) = p.span {
        a["timestamps"] = json!({ "start": start, "end": end });
    }
    if let Some(url) = &p.url {
        a["buttons"] = json!([{ "label": "Listen Along", "url": url }]);
    }
    a
}

fn set(conn: &mut Conn, p: Option<&Presence>, display_type: bool) -> std::io::Result<Value> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0).to_string();
    let args = match p {
        Some(p) => json!({ "pid": std::process::id(), "activity": activity(p, display_type) }),
        None => json!({ "pid": std::process::id() }),
    };
    frame(&mut conn.stream, 1, &json!({ "cmd": "SET_ACTIVITY", "args": args, "nonce": nonce }))?;
    // Read the answer (and skip unrelated events) so the socket never backs up.
    for _ in 0..4 {
        let (op, v) = read_frame(&mut conn.stream)?;
        if op == 2 {
            return Err(std::io::Error::other("closed by Discord"));
        }
        if v.get("nonce").and_then(Value::as_str) == Some(nonce.as_str()) {
            return Ok(v);
        }
    }
    Ok(Value::Null)
}

fn spawn() -> mpsc::Sender<Msg> {
    let (tx, rx) = mpsc::channel::<Msg>();
    std::thread::spawn(move || {
        let mut conn: Option<Conn> = None;
        let mut want: Option<Presence> = None;
        let mut client = String::new();
        let mut dirty = false;
        // Older Discord builds reject fields they don't know; then it is left out.
        let mut display_type = true;
        loop {
            let timeout = if dirty { Duration::from_secs(15) } else { Duration::from_secs(3600) };
            match rx.recv_timeout(timeout) {
                Ok(Msg::Set(p, id)) => {
                    want = p;
                    if id != client {
                        client = id;
                        conn = None;
                    }
                    dirty = true;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            // Take the newest message only.
            while let Ok(Msg::Set(p, id)) = rx.try_recv() {
                want = p;
                dirty = true;
                if id != client {
                    client = id;
                    conn = None;
                }
            }
            if !dirty || client.is_empty() {
                dirty = false;
                continue;
            }
            if conn.as_ref().is_some_and(|c| c.client != client) {
                conn = None;
            }
            if conn.is_none() {
                if want.is_none() {
                    // Nothing to show and not connected: nothing to clear either.
                    dirty = false;
                    continue;
                }
                conn = connect(&client);
            }
            let Some(c) = conn.as_mut() else { continue };
            match set(c, want.as_ref(), display_type) {
                Ok(v) if v.get("evt").and_then(Value::as_str) == Some("ERROR") => {
                    if display_type {
                        display_type = false;
                        let _ = set(c, want.as_ref(), false);
                    } else {
                        eprintln!("discord: {v}");
                    }
                    dirty = false;
                }
                Ok(_) => dirty = false,
                Err(e) => {
                    eprintln!("discord: {e}");
                    conn = None; // reconnect on the next round
                }
            }
        }
    });
    tx
}
