// Scrobbling to Last.fm and ListenBrainz.
//
// The UI thread reports what plays (`track`, `playing`, `tick`); a track counts as listened
// once it has really played for half its length or 4 minutes, whichever comes first, and
// only tracks longer than 30 s count (the rules both services use). Network calls run on one
// worker thread; scrobbles that fail (offline, service down) wait in
// ~/.config/applejack/scrobbles.json and go out with the next successful call.
use std::{
    cell::RefCell,
    collections::BTreeMap,
    path::PathBuf,
    sync::{LazyLock, mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use gtk::glib;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{model::Item, settings};

const LASTFM_API: &str = "https://ws.audioscrobbler.com/2.0/";
const LISTENBRAINZ_API: &str = "https://api.listenbrainz.org";

/// API root of the ListenBrainz server in use, e.g. https://api.listenbrainz.org/1.
fn lb_api(server: &str) -> String {
    let s = server.trim().trim_end_matches('/');
    let base = if s.is_empty() { LISTENBRAINZ_API } else { s };
    if base.ends_with("/1") { base.to_owned() } else { format!("{base}/1") }
}

/// API key and secret baked into release builds (APPLEJACK_LASTFM_KEY / _SECRET at build
/// time); Preferences can override them with the user's own.
pub fn lastfm_credentials() -> (String, String) {
    settings::with(|s| {
        let key = if s.lastfm_key.is_empty() { option_env!("APPLEJACK_LASTFM_KEY").unwrap_or("") } else { &s.lastfm_key };
        let secret =
            if s.lastfm_secret.is_empty() { option_env!("APPLEJACK_LASTFM_SECRET").unwrap_or("") } else { &s.lastfm_secret };
        (key.to_owned(), secret.to_owned())
    })
}

static AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .user_agent(concat!("Applejack/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
});

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Listen {
    pub artist: String,
    pub title: String,
    pub album: Option<String>,
    pub duration: Option<u64>,
    /// Unix time the track started.
    pub at: u64,
    /// Which services still need it: "lastfm", "listenbrainz".
    #[serde(default)]
    pub to: Vec<String>,
}

enum Job {
    NowPlaying(Listen),
    Scrobble(Listen),
}

// ---- UI-thread side ---------------------------------------------------------------------------

struct Current {
    id: String,
    listen: Listen,
    played: f64,
    last: Option<Instant>,
    done: bool,
    now_playing_sent: bool,
}

thread_local! {
    static CUR: RefCell<Option<Current>> = const { RefCell::new(None) };
    static WORKER: mpsc::Sender<(Job, Creds)> = spawn_worker();
}

fn targets() -> Vec<String> {
    let mut to = Vec::new();
    settings::with(|s| {
        if s.lastfm_enabled && !s.lastfm_session.is_empty() {
            to.push("lastfm".into());
        }
        if s.listenbrainz_enabled && !s.listenbrainz_token.is_empty() {
            to.push("listenbrainz".into());
        }
    });
    to
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// A (possibly) new track is on: starts counting it.
pub fn track(it: Option<&Item>, duration: f64) {
    CUR.with(|c| {
        let mut c = c.borrow_mut();
        let Some(it) = it else {
            *c = None;
            return;
        };
        let id = it.song_play_id();
        if let Some(cur) = c.as_mut().filter(|cur| cur.id == id) {
            if duration > 0.0 && cur.listen.duration.is_none() {
                cur.listen.duration = Some(duration.round() as u64);
            }
            return;
        }
        let artist = it.artist().trim().to_owned();
        if artist.is_empty() || it.kind() == crate::model::Kind::Station {
            *c = None;
            return;
        }
        *c = Some(Current {
            id,
            listen: Listen {
                artist,
                title: it.title().to_owned(),
                album: it.album.clone().filter(|a| !a.is_empty()),
                duration: it.duration.or(Some(duration)).filter(|d| *d > 0.0).map(|d| d.round() as u64),
                at: now_unix(),
                to: Vec::new(),
            },
            played: 0.0,
            last: None,
            done: false,
            now_playing_sent: false,
        });
    });
}

/// Playback started or stopped.
pub fn playing(on: bool) {
    CUR.with(|c| {
        let mut c = c.borrow_mut();
        let Some(cur) = c.as_mut() else { return };
        account(cur);
        cur.last = on.then(Instant::now);
        if on && !cur.now_playing_sent {
            cur.now_playing_sent = true;
            let to = targets();
            if !to.is_empty() {
                send(Job::NowPlaying(Listen { to, ..cur.listen.clone() }));
            }
        }
    });
}

/// Called a few times a second: scrobbles once enough of the track has played.
pub fn tick() {
    CUR.with(|c| {
        let mut c = c.borrow_mut();
        let Some(cur) = c.as_mut() else { return };
        if cur.done || cur.last.is_none() {
            return;
        }
        account(cur);
        cur.last = Some(Instant::now());
        let Some(len) = cur.listen.duration.filter(|d| *d > 30) else { return };
        if cur.played >= (len as f64 / 2.0).min(240.0) {
            cur.done = true;
            let to = targets();
            if !to.is_empty() {
                send(Job::Scrobble(Listen { to, ..cur.listen.clone() }));
            }
        }
    });
}

fn account(cur: &mut Current) {
    if let Some(last) = cur.last {
        // A long gap means the computer slept: that time was not listened to.
        let dt = last.elapsed().as_secs_f64();
        if dt < 5.0 {
            cur.played += dt;
        }
    }
}

/// The settings live on the UI thread: credentials travel with each job.
fn send(job: Job) {
    let c = creds();
    WORKER.with(|w| {
        let _ = w.send((job, c));
    });
}

// ---- worker thread ----------------------------------------------------------------------------

#[derive(Clone)]
struct Creds {
    lastfm_key: String,
    lastfm_secret: String,
    lastfm_session: String,
    listenbrainz_token: String,
    listenbrainz_url: String,
}

fn creds() -> Creds {
    let (lastfm_key, lastfm_secret) = lastfm_credentials();
    settings::with(|s| Creds {
        lastfm_key,
        lastfm_secret,
        lastfm_session: s.lastfm_session.clone(),
        listenbrainz_token: s.listenbrainz_token.clone(),
        listenbrainz_url: s.listenbrainz_url.clone(),
    })
}

fn queue_path() -> PathBuf {
    settings::config_dir().join("scrobbles.json")
}

fn spawn_worker() -> mpsc::Sender<(Job, Creds)> {
    let (tx, rx) = mpsc::channel::<(Job, Creds)>();
    std::thread::spawn(move || {
        let mut pending: Vec<Listen> =
            std::fs::read(queue_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        while let Ok((job, creds)) = rx.recv() {
            match job {
                Job::NowPlaying(l) => {
                    for to in &l.to {
                        let r = match to.as_str() {
                            "lastfm" => lastfm_now_playing(&creds, &l),
                            _ => listenbrainz_submit(&creds, "playing_now", std::slice::from_ref(&l)),
                        };
                        if let Err(e) = r {
                            eprintln!("now playing ({to}): {e}");
                        }
                    }
                }
                Job::Scrobble(l) => {
                    pending.push(l);
                    flush(&creds, &mut pending);
                    save_queue(&pending);
                }
            }
        }
    });
    tx
}

fn save_queue(pending: &[Listen]) {
    let path = queue_path();
    if pending.is_empty() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    if let Ok(text) = serde_json::to_vec(pending) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &path)).is_err() {
            eprintln!("couldn’t save {}", path.display());
        }
    }
}

/// Sends every waiting scrobble to each service it still needs; keeps the ones that failed.
fn flush(creds: &Creds, pending: &mut Vec<Listen>) {
    // Last.fm takes up to 50 per call; it ignores scrobbles older than two weeks.
    let lastfm: Vec<Listen> = pending.iter().filter(|l| l.to.iter().any(|t| t == "lastfm")).cloned().collect();
    for chunk in lastfm.chunks(50) {
        match lastfm_scrobble(creds, chunk) {
            Ok(()) => mark_done(pending, chunk, "lastfm"),
            Err(Fail::Rejected(e)) => {
                eprintln!("last.fm dropped scrobbles: {e}");
                mark_done(pending, chunk, "lastfm");
            }
            Err(Fail::Retry(e)) => {
                eprintln!("last.fm: {e}");
                break;
            }
        }
    }
    let lb: Vec<Listen> = pending.iter().filter(|l| l.to.iter().any(|t| t == "listenbrainz")).cloned().collect();
    for chunk in lb.chunks(100) {
        match listenbrainz_submit(creds, if chunk.len() == 1 { "single" } else { "import" }, chunk) {
            Ok(()) => mark_done(pending, chunk, "listenbrainz"),
            Err(Fail::Rejected(e)) => {
                eprintln!("listenbrainz dropped listens: {e}");
                mark_done(pending, chunk, "listenbrainz");
            }
            Err(Fail::Retry(e)) => {
                eprintln!("listenbrainz: {e}");
                break;
            }
        }
    }
    pending.retain(|l| !l.to.is_empty());
    // Two weeks: neither service takes older listens as scrobbles.
    let cutoff = now_unix().saturating_sub(14 * 24 * 3600);
    pending.retain(|l| l.at >= cutoff);
}

fn mark_done(pending: &mut [Listen], sent: &[Listen], service: &str) {
    for l in pending.iter_mut() {
        if sent.iter().any(|s| s.at == l.at && s.title == l.title && s.artist == l.artist) {
            l.to.retain(|t| t != service);
        }
    }
}

enum Fail {
    /// Network or server trouble: try again later.
    Retry(String),
    /// The service refused these (bad data, bad credentials): don't keep them.
    Rejected(String),
}

impl std::fmt::Display for Fail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fail::Retry(e) | Fail::Rejected(e) => f.write_str(e),
        }
    }
}

// ---- Last.fm ----------------------------------------------------------------------------------

/// Signs a Last.fm call: md5 of the sorted key/value pairs followed by the secret.
fn lastfm_sign(params: &mut BTreeMap<String, String>, secret: &str) {
    let mut base = String::new();
    for (k, v) in params.iter() {
        if k != "format" && k != "callback" {
            base.push_str(k);
            base.push_str(v);
        }
    }
    base.push_str(secret);
    let sig = glib::compute_checksum_for_string(glib::ChecksumType::Md5, &base).map(|s| s.to_string()).unwrap_or_default();
    params.insert("api_sig".into(), sig);
}

/// POSTs a signed call and returns the JSON answer.
pub fn lastfm_call(key: &str, secret: &str, method: &str, extra: &[(&str, String)]) -> Result<Value, (i64, String)> {
    if key.is_empty() || secret.is_empty() {
        return Err((0, "no Last.fm API key".into()));
    }
    let mut params: BTreeMap<String, String> = extra.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    params.insert("method".into(), method.into());
    params.insert("api_key".into(), key.into());
    lastfm_sign(&mut params, secret);
    params.insert("format".into(), "json".into());
    let form: Vec<(&str, &str)> = params.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let mut resp = AGENT.post(LASTFM_API).send_form(form).map_err(|e| (-1, e.to_string()))?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().map_err(|e| (-1, e.to_string()))?;
    let v: Value = serde_json::from_str(&text).map_err(|_| (-1, format!("HTTP {status}")))?;
    if let Some(code) = v.get("error").and_then(Value::as_i64) {
        return Err((code, v.get("message").and_then(Value::as_str).unwrap_or("error").to_owned()));
    }
    if status >= 400 {
        return Err((-1, format!("HTTP {status}")));
    }
    Ok(v)
}

fn lastfm_fail((code, msg): (i64, String)) -> Fail {
    // 9 = invalid session, 4/10/26 = bad API key, 6 = bad parameters; 11/16/29 and network = retry.
    match code {
        4 | 6 | 9 | 10 | 13 | 26 => Fail::Rejected(format!("{msg} ({code})")),
        _ => Fail::Retry(format!("{msg} ({code})")),
    }
}

fn lastfm_now_playing(c: &Creds, l: &Listen) -> Result<(), Fail> {
    let mut p = vec![("artist", l.artist.clone()), ("track", l.title.clone()), ("sk", c.lastfm_session.clone())];
    if let Some(a) = &l.album {
        p.push(("album", a.clone()));
    }
    if let Some(d) = l.duration {
        p.push(("duration", d.to_string()));
    }
    lastfm_call(&c.lastfm_key, &c.lastfm_secret, "track.updateNowPlaying", &p).map(|_| ()).map_err(lastfm_fail)
}

fn lastfm_scrobble(c: &Creds, batch: &[Listen]) -> Result<(), Fail> {
    let mut p: Vec<(&str, String)> = vec![("sk", c.lastfm_session.clone())];
    let mut keys: Vec<(String, String)> = Vec::new();
    for (i, l) in batch.iter().enumerate() {
        keys.push((format!("artist[{i}]"), l.artist.clone()));
        keys.push((format!("track[{i}]"), l.title.clone()));
        keys.push((format!("timestamp[{i}]"), l.at.to_string()));
        if let Some(a) = &l.album {
            keys.push((format!("album[{i}]"), a.clone()));
        }
        if let Some(d) = l.duration {
            keys.push((format!("duration[{i}]"), d.to_string()));
        }
    }
    for (k, v) in &keys {
        p.push((k.as_str(), v.clone()));
    }
    lastfm_call(&c.lastfm_key, &c.lastfm_secret, "track.scrobble", &p).map(|_| ()).map_err(lastfm_fail)
}

/// Connecting an account, step 1: a request token and the page where the user allows access.
pub fn lastfm_begin_auth() -> Result<(String, String), String> {
    let (key, secret) = lastfm_credentials();
    let v = lastfm_call(&key, &secret, "auth.getToken", &[]).map_err(|(_, e)| e)?;
    let token = v.get("token").and_then(Value::as_str).ok_or("no token in the answer")?.to_owned();
    let url = format!("https://www.last.fm/api/auth/?api_key={key}&token={token}");
    Ok((token, url))
}

/// Step 2, polled until the user has allowed access: (session key, user name).
/// Err(None) = not allowed yet.
pub fn lastfm_finish_auth(token: &str) -> Result<(String, String), Option<String>> {
    let (key, secret) = lastfm_credentials();
    match lastfm_call(&key, &secret, "auth.getSession", &[("token", token.to_owned())]) {
        Ok(v) => {
            let s = v.get("session").ok_or(Some("no session in the answer".to_owned()))?;
            let sk = s.get("key").and_then(Value::as_str).unwrap_or_default().to_owned();
            let name = s.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
            Ok((sk, name))
        }
        // 14 = token not authorized yet
        Err((14, _)) => Err(None),
        Err((_, e)) => Err(Some(e)),
    }
}

// ---- ListenBrainz -------------------------------------------------------------------------------

fn lb_payload(l: &Listen, with_time: bool) -> Value {
    let mut info = json!({
        "media_player": "Applejack",
        "submission_client": "Applejack",
        "submission_client_version": env!("CARGO_PKG_VERSION"),
        "music_service": "music.apple.com",
    });
    if let Some(d) = l.duration {
        info["duration_ms"] = json!(d * 1000);
    }
    let mut meta = json!({ "artist_name": l.artist, "track_name": l.title, "additional_info": info });
    if let Some(a) = &l.album {
        meta["release_name"] = json!(a);
    }
    let mut p = json!({ "track_metadata": meta });
    if with_time {
        p["listened_at"] = json!(l.at);
    }
    p
}

fn listenbrainz_submit(c: &Creds, kind: &str, listens: &[Listen]) -> Result<(), Fail> {
    if c.listenbrainz_token.is_empty() {
        return Err(Fail::Rejected("no ListenBrainz token".into()));
    }
    let body = json!({
        "listen_type": kind,
        "payload": listens.iter().map(|l| lb_payload(l, kind != "playing_now")).collect::<Vec<_>>(),
    });
    let mut resp = AGENT
        .post(format!("{}/submit-listens", lb_api(&c.listenbrainz_url)))
        .header("Authorization", format!("Token {}", c.listenbrainz_token))
        .header("Content-Type", "application/json")
        .send(body.to_string())
        .map_err(|e| Fail::Retry(e.to_string()))?;
    let status = resp.status().as_u16();
    if status == 200 {
        return Ok(());
    }
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    let msg = format!("HTTP {status}: {}", text.chars().take(200).collect::<String>());
    if status == 429 || status >= 500 { Err(Fail::Retry(msg)) } else { Err(Fail::Rejected(msg)) }
}

/// Checks a user token: the account name it belongs to.
pub fn listenbrainz_validate(server: &str, token: &str) -> Result<String, String> {
    let mut resp = AGENT
        .get(format!("{}/validate-token", lb_api(server)))
        .header("Authorization", format!("Token {}", token.trim()))
        .call()
        .map_err(|e| e.to_string())?;
    let text = resp.body_mut().read_to_string().map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if v.get("valid").and_then(Value::as_bool) == Some(true) {
        Ok(v.get("user_name").and_then(Value::as_str).unwrap_or_default().to_owned())
    } else {
        Err(v.get("message").and_then(Value::as_str).unwrap_or("invalid token").to_owned())
    }
}

/// Runs blocking work on a thread and hands the result back to the UI thread.
pub async fn off_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = tx.send_blocking(f());
    });
    rx.recv().await.expect("worker thread result")
}
