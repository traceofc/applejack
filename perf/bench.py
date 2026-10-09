#!/usr/bin/env python3
"""Applejack performance baseline: CPU, memory and GPU load per scenario.

Launches the release build of the GTK client once per scenario and run, drives it with the
AM_GTK_* dev variables (see App::dev_snapshot in ui/src/ui.rs), and samples every process of
the app from /proc:

  cpu   utime+stime of each process, in % of one core (like top)
  pss   proportional set size from /proc/<pid>/smaps_rollup (shared pages split fairly)
  gpu   busy time of the process's DRM clients (drm-engine-gfx in /proc/<pid>/fdinfo), in %
        of the GPU's time; amdgpu, i915 and xe report it
  sys   the whole GPU's gpu_busy_percent (amdgpu) and the compositor's CPU / GPU, which
        draws the window and so pays for our frames too

By default everything runs inside a private headless mutter (see start_headless), so the
compositor numbers are ours alone; --desktop uses the current session instead.

Processes are grouped as ui (the GTK client), engine (Electron main), renderer (the hidden
music.apple.com page), gpu (Electron's GPU process), audio/network/other (utility processes).

Usage:  python3 perf/bench.py [--runs 2] [--warmup 18] [--measure 30] [--only a,b] [--out perf/results]
Writes <out>/samples.csv (one row per process group per sample) and <out>/runs.json.
Playback runs at volume 0.002 (−54 dB, inaudible; the analyzer compensates for it). Uses a copy of ~/.config/applejack for the engine profile so a
running client is not disturbed. Only the Python standard library is needed.
"""

import argparse
import csv
import glob
import json
import os
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BIN = ROOT / "ui" / "target" / "release" / "applejack"
HZ = os.sysconf("SC_CLK_TCK")
PERIOD = 0.5

# The EQ test track the user picked in Preferences, a real catalog song.
SONG = "1827768769"

SCENARIOS = {
    # name: (label, env)
    "idle_home": ("Idle, Home", {"AM_GTK_PAGE": "home"}),
    "scroll_home": ("Scrolling Home (art shelves)", {"AM_GTK_PAGE": "home", "AM_GTK_SCROLL": "1500"}),
    "scroll_songs": ("Scrolling Library → Songs", {"AM_GTK_PAGE": "lib:songs", "AM_GTK_SCROLL": "1500"}),
    "play": ("Playback", {"AM_GTK_PAGE": "home", "AM_GTK_PLAY": SONG}),
    "play_lyrics": ("Playback + synced lyrics", {"AM_GTK_PAGE": "home", "AM_GTK_PLAY": SONG, "AM_GTK_PANEL": "lyrics"}),
    "eq_play": ("Playback + EQ with analyzer", {"AM_GTK_PAGE": "home", "AM_GTK_EQ": "1", "AM_GTK_PLAY": SONG}),
}


def read(path):
    try:
        with open(path) as f:
            return f.read()
    except OSError:
        return None


def children(pid):
    out = []
    for p in glob.glob(f"/proc/{pid}/task/*/children"):
        out += [int(x) for x in (read(p) or "").split()]
    return out


def tree(pid):
    todo, seen = [pid], []
    while todo:
        p = todo.pop()
        seen.append(p)
        todo += children(p)
    return seen


def role(pid, ui_pid):
    if pid == ui_pid:
        return "ui"
    # Chromium rewrites its title, so the arguments come back as one space-joined string.
    args = (read(f"/proc/{pid}/cmdline") or "").replace("\0", " ").split()
    t = next((a.split("=", 1)[1] for a in args if a.startswith("--type=")), None)
    if t is None:
        return "engine"
    if t == "renderer":
        return "renderer"
    if t == "gpu-process":
        return "gpu"
    if t == "utility":
        sub = next((a for a in args if a.startswith("--utility-sub-type=")), "")
        if "audio" in sub.lower():
            return "audio"
        if "network" in sub.lower():
            return "network"
    return "other"


def cpu_ticks(pid):
    s = read(f"/proc/{pid}/stat")
    if not s:
        return None
    f = s[s.rfind(")") + 2:].split()
    return int(f[11]) + int(f[12])


def pss_kb(pid):
    s = read(f"/proc/{pid}/smaps_rollup") or ""
    for line in s.splitlines():
        if line.startswith("Pss:"):
            return int(line.split()[1])
    return 0


def gpu_ns(pid):
    """Sum of drm-engine-* busy ns over the process's distinct DRM clients."""
    clients = {}
    for fd in glob.glob(f"/proc/{pid}/fdinfo/*"):
        s = read(fd)
        if not s or "drm-client-id" not in s:
            continue
        cid, total = None, 0
        for line in s.splitlines():
            if line.startswith("drm-client-id:"):
                cid = line.split()[1]
            elif line.startswith("drm-engine-") and line.endswith(" ns"):
                total += int(line.split()[1])
        if cid is not None:
            clients[cid] = max(clients.get(cid, 0), total)
    return sum(clients.values())


def gpu_busy():
    vals = [int(v) for p in glob.glob("/sys/class/drm/card*/device/gpu_busy_percent") if (v := read(p)) and v.strip().isdigit()]
    return max(vals) if vals else None


def compositor_pid():
    for name in ("gnome-shell", "kwin_wayland", "sway", "Hyprland", "mutter"):
        r = subprocess.run(["pgrep", "-x", "-o", name], capture_output=True, text=True)
        if r.stdout.strip():
            return int(r.stdout.split()[0])
    return None


def start_headless(log_dir):
    """A private headless mutter with one 60 Hz virtual monitor. On the real desktop a window
    started from a terminal opens behind the focused one, the compositor stops sending it
    frames and every GUI number reads zero; here the window is always on screen and the
    user's own apps (a browser playing video, ...) stay out of the compositor numbers."""
    name = f"applejack-bench-{os.getpid()}"
    log = open(log_dir / "mutter.log", "w")
    proc = subprocess.Popen(["mutter", "--headless", "--wayland", "--no-x11", "--virtual-monitor", "1600x1000@60",
                             "--wayland-display", name], stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
    sock = Path(os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}")) / name
    for _ in range(50):
        if sock.exists():
            return proc, name
        time.sleep(0.1)
    proc.kill()
    sys.exit("headless mutter did not start, see " + str(log_dir / "mutter.log"))


def prepare_profile(scratch):
    src = Path.home() / ".config" / "applejack"
    dst = scratch / "profile"
    if not dst.exists():
        shutil.copytree(src, dst, symlinks=True, ignore=shutil.ignore_patterns("Singleton*", "Crashpad"))
    return dst


def run_one(name, run, args, profile, writer, log_dir, display):
    label, extra = SCENARIOS[name]
    env = dict(os.environ)
    if display:
        env.pop("DISPLAY", None)
        env["WAYLAND_DISPLAY"] = display[1]
    env.update(extra)
    env.update({
        "AM_ENGINE_PROFILE": str(profile),
        "AM_GTK_SNAPSHOT": str(log_dir / f"{name}-{run}.png"),
        "AM_GTK_DELAY": str(args.warmup + args.measure + 2),
        "AM_GTK_WIDTH": "1400",
        "AM_GTK_HEIGHT": "900",
        # −54 dB: inaudible, but not 0, since the engine's analyzer goes quiet at volume 0.
        "AM_GTK_VOLUME": "0.002",
        "AM_EQ_FPS": "1",
    })
    log = open(log_dir / f"{name}-{run}.log", "w")
    proc = subprocess.Popen([str(BIN)], env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
    comp = display[0].pid if display else compositor_pid()
    t0 = time.monotonic()
    prev = {}
    n = 0
    try:
        while True:
            now = time.monotonic()
            t = now - t0
            if t > args.warmup + args.measure or proc.poll() is not None:
                break
            pids = tree(proc.pid)
            groups = {}
            for pid in pids:
                ticks, g = cpu_ticks(pid), gpu_ns(pid)
                if ticks is None:
                    continue
                r = role(pid, proc.pid)
                p = prev.get(pid)
                prev[pid] = (now, ticks, g)
                agg = groups.setdefault(r, [0.0, 0, 0.0, 0])
                agg[1] += pss_kb(pid)
                agg[3] += 1
                if p:
                    dt = now - p[0]
                    agg[0] += 100.0 * (ticks - p[1]) / HZ / dt
                    agg[2] += 100.0 * (g - p[2]) / 1e9 / dt
            if comp:
                ticks, g = cpu_ticks(comp), gpu_ns(comp)
                p = prev.get(("comp", comp))
                prev[("comp", comp)] = (now, ticks, g)
                if p and ticks is not None:
                    dt = now - p[0]
                    groups["compositor"] = [100.0 * (ticks - p[1]) / HZ / dt, pss_kb(comp), 100.0 * (g - p[2]) / 1e9 / dt, 1]
            busy = gpu_busy()
            phase = "warmup" if t < args.warmup else "measure"
            for r, (cpu, pss, gpu, cnt) in groups.items():
                writer.writerow([name, run, f"{t:.2f}", phase, r, f"{cpu:.2f}", pss, f"{gpu:.3f}", cnt, busy if busy is not None else ""])
            n += 1
            time.sleep(max(0.0, PERIOD - (time.monotonic() - now)))
    finally:
        try:
            os.killpg(proc.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGKILL)
        log.close()
    # AM_EQ_FPS: the EQ graph prints "eq: N ticks/s, M draws/s" once a second while it is open.
    fps = []
    for line in (read(log_dir / f"{name}-{run}.log") or "").splitlines():
        if line.startswith("eq: ") and "draws/s" in line:
            a, b = line[4:].split(",")
            fps.append((int(a.split()[0]), int(b.split()[0])))
    fps = fps[-args.measure:]
    return n, ([sum(x[0] for x in fps) / len(fps), sum(x[1] for x in fps) / len(fps)] if fps else None)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--runs", type=int, default=2)
    ap.add_argument("--warmup", type=int, default=18)
    ap.add_argument("--measure", type=int, default=30)
    ap.add_argument("--only", default="")
    ap.add_argument("--desktop", action="store_true", help="run on the current desktop instead of a headless mutter")
    ap.add_argument("--out", default=str(ROOT / "perf" / "results"))
    ap.add_argument("--scratch", default=os.environ.get("TMPDIR", "/tmp") + "/applejack-bench")
    args = ap.parse_args()
    if not BIN.is_file():
        sys.exit(f"build first: (cd ui && cargo build --release); missing {BIN}")
    names = [n for n in SCENARIOS if not args.only or n in args.only.split(",")]
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    scratch = Path(args.scratch)
    log_dir = scratch / "logs"
    log_dir.mkdir(parents=True, exist_ok=True)
    profile = prepare_profile(scratch)
    display = None if args.desktop else start_headless(log_dir)
    eq_fps = {}
    with open(out / "samples.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["scenario", "run", "t", "phase", "group", "cpu_pct", "pss_kb", "gpu_pct", "procs", "gpu_busy_pct"])
        # Round-robin so slow drift (thermals, background jobs) spreads over every scenario.
        for run in range(1, args.runs + 1):
            for name in names:
                print(f"[run {run}/{args.runs}] {name} …", flush=True)
                n, fps = run_one(name, run, args, profile, w, log_dir, display)
                f.flush()
                if fps:
                    eq_fps.setdefault(name, []).append(fps)
                print(f"    {n} samples, eq ticks/draws per s: {fps}", flush=True)
                time.sleep(3)
    if display:
        display[0].terminate()
        display[0].wait(timeout=10)
    meta = {
        "display": "current desktop" if args.desktop else "headless mutter, 1600x1000@60 virtual monitor",
        "date": time.strftime("%Y-%m-%d %H:%M"),
        "runs": args.runs, "warmup_s": args.warmup, "measure_s": args.measure, "period_s": PERIOD,
        "scenarios": {n: SCENARIOS[n][0] for n in names},
        "cpu": next((l.split(":", 1)[1].strip() for l in (read("/proc/cpuinfo") or "").splitlines() if l.startswith("model name")), ""),
        "cores": os.cpu_count(),
        "kernel": os.uname().release,
        "gpu": subprocess.run(["sh", "-c", "lspci | grep -iE 'vga|3d' | cut -d: -f3-"], capture_output=True, text=True).stdout.strip(),
        "logs": str(log_dir),
        "eq_ticks_draws_per_s": eq_fps,
    }
    (out / "runs.json").write_text(json.dumps(meta, indent=2, ensure_ascii=False))
    print("done:", out)


if __name__ == "__main__":
    main()
