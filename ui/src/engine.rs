// Client for applejack-engine (castLabs Electron + hidden music.apple.com page).
// Spawns the engine as a child process and speaks JSON-lines RPC over stdio:
//   -> {"id":1,"method":"play","params":{...}}
//   <- {"id":1,"result":...} | {"id":1,"error":{"message":...}} | {"event":"state","data":{...}}
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    fs::File,
    future::Future,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    rc::Rc,
    time::{Duration, Instant},
};

use gtk::glib;
use serde_json::{Value, json};

type Reply = Result<Value, String>;
type EventFn = Box<dyn Fn(&str, &Value)>;

enum Msg {
    Line(u64, String),
    Exited(u64),
}

pub struct Engine {
    dir: Option<PathBuf>,
    log_path: PathBuf,
    generation: Cell<u64>,
    child: RefCell<Option<Child>>,
    stdin: RefCell<Option<ChildStdin>>,
    pending: RefCell<HashMap<u64, async_channel::Sender<Reply>>>,
    next_id: Cell<u64>,
    on_event: RefCell<Option<EventFn>>,
    tx: async_channel::Sender<Msg>,
}

impl Engine {
    pub fn new(dir: Option<PathBuf>, log_path: PathBuf) -> Rc<Self> {
        let (tx, rx) = async_channel::unbounded::<Msg>();
        let engine = Rc::new(Self {
            dir,
            log_path,
            generation: Cell::new(0),
            child: RefCell::new(None),
            stdin: RefCell::new(None),
            pending: RefCell::new(HashMap::new()),
            next_id: Cell::new(1),
            on_event: RefCell::new(None),
            tx,
        });
        let weak = Rc::downgrade(&engine);
        glib::spawn_future_local(async move {
            while let Ok(msg) = rx.recv().await {
                let Some(engine) = weak.upgrade() else { break };
                engine.dispatch(msg);
            }
        });
        engine
    }

    pub fn set_event_handler(&self, f: impl Fn(&str, &Value) + 'static) {
        *self.on_event.borrow_mut() = Some(Box::new(f));
    }

    /// (Re)starts the engine process.
    pub fn start(&self) -> Result<(), String> {
        self.stop();
        let dir = self.dir.as_ref().ok_or("engine folder not found (set AM_ENGINE_DIR or run ./install.sh)")?;
        let electron = electron_binary(dir)?;
        let log = File::create(&self.log_path).map_err(|e| format!("{}: {e}", self.log_path.display()))?;
        let mut child = Command::new(&electron)
            .arg(dir.join("engine").join("main.js"))
            .current_dir(dir)
            // Inherited from another Electron app, this makes the zygote crash at startup.
            .env_remove("CHROME_DESKTOP")
            .env_remove("ELECTRON_RUN_AS_NODE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .spawn()
            .map_err(|e| format!("{}: {e}", electron.display()))?;

        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let stdout = child.stdout.take().expect("piped stdout");
        *self.stdin.borrow_mut() = child.stdin.take();
        *self.child.borrow_mut() = Some(child);

        let tx = self.tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send_blocking(Msg::Line(generation, line)).is_err() {
                    return;
                }
            }
            let _ = tx.send_blocking(Msg::Exited(generation));
        });
        Ok(())
    }

    /// Asks the engine to quit, then makes sure it is gone.
    pub fn stop(&self) {
        let _ = self.write(&json!({ "id": null, "method": "quit" }));
        self.stdin.borrow_mut().take(); // EOF on stdin also makes it quit
        if let Some(mut child) = self.child.borrow_mut().take() {
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                if let Ok(Some(_)) = child.try_wait() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        self.fail_pending("engine stopped");
    }

    pub fn call(&self, method: &str, params: Value) -> impl Future<Output = Reply> + 'static {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        let (tx, rx) = async_channel::bounded(1);
        let sent = self.write(&json!({ "id": id, "method": method, "params": params }));
        if sent.is_ok() {
            self.pending.borrow_mut().insert(id, tx);
        }
        async move {
            sent?;
            rx.recv().await.unwrap_or_else(|_| Err("engine stopped".into()))
        }
    }

    fn write(&self, msg: &Value) -> Result<(), String> {
        let mut stdin = self.stdin.borrow_mut();
        let stdin = stdin.as_mut().ok_or("engine is not running")?;
        writeln!(stdin, "{msg}").and_then(|_| stdin.flush()).map_err(|e| format!("engine: {e}"))
    }

    fn fail_pending(&self, why: &str) {
        let pending: Vec<_> = self.pending.borrow_mut().drain().collect();
        for (_, tx) in pending {
            let _ = tx.try_send(Err(why.to_owned()));
        }
    }

    fn dispatch(&self, msg: Msg) {
        match msg {
            Msg::Line(generation, line) if generation == self.generation.get() => {
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    eprintln!("engine: {line}");
                    return;
                };
                if let Some(id) = v.get("id").and_then(Value::as_u64) {
                    let tx = self.pending.borrow_mut().remove(&id);
                    if let Some(tx) = tx {
                        let reply = match v.get("error") {
                            Some(e) if !e.is_null() => Err(e
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or("engine error")
                                .to_owned()),
                            _ => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
                        };
                        let _ = tx.try_send(reply);
                    }
                } else if let Some(event) = v.get("event").and_then(Value::as_str) {
                    let data = v.get("data").cloned().unwrap_or(Value::Null);
                    self.emit(event, &data);
                }
            }
            Msg::Exited(generation) if generation == self.generation.get() => {
                self.stdin.borrow_mut().take();
                if let Some(mut child) = self.child.borrow_mut().take() {
                    let _ = child.wait();
                }
                self.fail_pending("engine exited");
                self.emit("exit", &Value::Null);
            }
            _ => {}
        }
    }

    fn emit(&self, event: &str, data: &Value) {
        if let Some(f) = self.on_event.borrow().as_ref() {
            f(event, data);
        }
    }
}

/// The engine folder holds engine/main.js and either electron/applejack-engine (installed)
/// or node_modules/electron (source tree).
pub fn find_engine_dir() -> Option<PathBuf> {
    let ok = |p: &Path| p.join("engine").join("main.js").is_file();
    if let Some(dir) = std::env::var_os("AM_ENGINE_DIR").map(PathBuf::from) {
        return ok(&dir).then_some(dir);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.ancestors().skip(1).find(|p| ok(p)) {
            return Some(dir.to_path_buf());
        }
    }
    let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    ok(&dev).then(|| dev.canonicalize().unwrap_or(dev))
}

fn electron_binary(dir: &Path) -> Result<PathBuf, String> {
    // Installed by install.sh: a copy of castLabs Electron with the binary renamed,
    // so the engine shows up as applejack-engine in ps / system monitor.
    let installed = dir.join("electron").join("applejack-engine");
    if installed.is_file() {
        return Ok(installed);
    }
    let pkg = dir.join("node_modules").join("electron");
    let rel = std::fs::read_to_string(pkg.join("path.txt")).unwrap_or_else(|_| "electron".into());
    let bin = pkg.join("dist").join(rel.trim());
    if bin.is_file() {
        Ok(bin)
    } else {
        Err(format!("castLabs Electron not installed ({}); run: npm run setup", bin.display()))
    }
}
