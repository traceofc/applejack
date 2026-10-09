#!/usr/bin/env python3
"""Charts and a summary table from perf/bench.py results (standard library only).

Usage: python3 perf/plot.py [--in perf/results]
Writes <in>/cpu.svg, memory.svg, gpu.svg, timeline.svg (plus .png when rsvg-convert is
installed) and <in>/table.md with the numbers behind them.
"""

import argparse
import csv
import json
import shutil
import subprocess
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Process groups from bench.py folded into five stacks, in fixed palette order.
STACKS = [
    ("ui", "Окно (GTK)", "#2a78d6", ["ui"]),
    ("renderer", "Движок: страница", "#eb6834", ["renderer"]),
    ("gpu", "Движок: GPU-процесс", "#1baf7a", ["gpu"]),
    ("rest", "Движок: остальное", "#eda100", ["engine", "audio", "network", "other"]),
    ("compositor", "Композитор", "#e87ba4", ["compositor"]),
]
SURFACE, INK, INK2, GRID = "#fcfcfb", "#0b0b0b", "#52514e", "#e4e3df"
FONT = "font-family='Inter, Cantarell, DejaVu Sans, sans-serif'"


def load(src):
    rows = list(csv.DictReader(open(src / "samples.csv")))
    meta = json.loads((src / "runs.json").read_text())
    return rows, meta


def aggregate(rows, metric):
    """{scenario: {stack: [per-run means]}} over the measure phase."""
    per = defaultdict(lambda: defaultdict(list))  # (scenario, run, stack) -> samples by t
    for r in rows:
        if r["phase"] != "measure":
            continue
        stack = next(k for k, _, _, groups in STACKS if r["group"] in groups)
        v = float(r[metric]) / (1024 if metric == "pss_kb" else 1)
        per[(r["scenario"], r["run"])][(stack, r["t"])].append(v)
    out = defaultdict(lambda: defaultdict(list))
    for (sc, run), d in per.items():
        by_stack = defaultdict(list)
        for (stack, _t), vals in d.items():
            by_stack[stack].append(sum(vals))
        for stack, _, _, _ in STACKS:
            vals = by_stack.get(stack, [])
            out[sc][stack].append(sum(vals) / len(vals) if vals else 0.0)
    return out


def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def stacked_bars(agg, labels, order, title, unit, path, fmt="{:.0f}"):
    w, row_h, left, right, top = 980, 46, 300, 90, 92
    h = top + row_h * len(order) + 56
    means = {sc: {k: sum(v) / len(v) for k, v in agg[sc].items()} for sc in order}
    totals = {sc: [sum(agg[sc][k][i] for k, *_ in STACKS) for i in range(len(agg[sc]["ui"]))] for sc in order}
    vmax = max(max(t) for t in totals.values()) * 1.08 or 1
    step = nice_step(vmax)
    x = lambda v: left + (w - left - right) * v / vmax
    s = [f"<svg xmlns='http://www.w3.org/2000/svg' width='{w}' height='{h}' viewBox='0 0 {w} {h}' {FONT}>",
         f"<rect width='{w}' height='{h}' fill='{SURFACE}'/>",
         f"<text x='24' y='34' font-size='19' font-weight='600' fill='{INK}'>{esc(title)}</text>"]
    lx = 24
    for _, name, color, _ in STACKS:  # legend, one row above the plot
        s.append(f"<rect x='{lx}' y='52' width='12' height='12' rx='3' fill='{color}'/>")
        s.append(f"<text x='{lx + 18}' y='62' font-size='13' fill='{INK2}'>{esc(name)}</text>")
        lx += 30 + 7.3 * len(name)
    v = 0.0
    while v <= vmax:
        s.append(f"<line x1='{x(v):.1f}' x2='{x(v):.1f}' y1='{top - 6}' y2='{top + row_h * len(order)}' stroke='{GRID}'/>")
        s.append(f"<text x='{x(v):.1f}' y='{top + row_h * len(order) + 20}' font-size='12' fill='{INK2}' text-anchor='middle'>{fmt.format(v)}</text>")
        v += step
    s.append(f"<text x='{(left + w - right) / 2}' y='{h - 12}' font-size='13' fill='{INK2}' text-anchor='middle'>{esc(unit)}</text>")
    for i, sc in enumerate(order):
        y = top + i * row_h + 8
        bh = row_h - 18
        s.append(f"<text x='{left - 12}' y='{y + bh / 2 + 5}' font-size='14' fill='{INK}' text-anchor='end'>{esc(labels[sc])}</text>")
        acc = 0.0
        segs = [(k, c, means[sc][k], n) for k, n, c, _ in STACKS if means[sc][k] > 0]
        for j, (k, c, val, n) in enumerate(segs):
            x0, x1 = x(acc), x(acc + val)
            acc += val
            wd = max(0.0, x1 - x0 - (2 if j < len(segs) - 1 else 0))  # 2px surface gap
            r = 4 if j == len(segs) - 1 else 0
            s.append(f"<g><title>{esc(labels[sc])} · {esc(n)}: {fmt.format(val)}</title>"
                     f"<rect x='{x0:.1f}' y='{y}' width='{wd:.1f}' height='{bh}' rx='{r}' fill='{c}'/>"
                     + (f"<rect x='{x0:.1f}' y='{y}' width='{min(wd, 4):.1f}' height='{bh}' fill='{c}'/>" if r and j else "")
                     + "</g>")
        lo, hi = min(totals[sc]), max(totals[sc])
        if hi - lo > 0:  # spread between runs
            ym = y + bh / 2
            s.append(f"<line x1='{x(lo):.1f}' x2='{x(hi):.1f}' y1='{ym}' y2='{ym}' stroke='{INK}' stroke-width='1.5'/>")
            for xv in (lo, hi):
                s.append(f"<line x1='{x(xv):.1f}' x2='{x(xv):.1f}' y1='{ym - 5}' y2='{ym + 5}' stroke='{INK}' stroke-width='1.5'/>")
        s.append(f"<text x='{x(max(acc, hi)) + 8:.1f}' y='{y + bh / 2 + 5}' font-size='13' font-weight='600' fill='{INK}'>{fmt.format(acc)}</text>")
    s.append("</svg>")
    path.write_text("\n".join(s))
    return means


def nice_step(vmax):
    for st in (1, 2, 5, 10, 20, 25, 50, 100, 200, 250, 500, 1000):
        if vmax / st <= 8:
            return st
    return 2000


def timeline(rows, labels, order, warmup, path):
    """Total CPU (all app processes + compositor) over time, run 1, one panel per scenario."""
    series = defaultdict(lambda: defaultdict(float))
    for r in rows:
        if r["run"] == "1":
            series[r["scenario"]][float(r["t"])] += float(r["cpu_pct"])
    cols, pw, ph, gap = 3, 300, 150, 28
    rows_n = (len(order) + cols - 1) // cols
    w, h = 24 + cols * (pw + gap), 70 + rows_n * (ph + 62)
    vmax = max(max(d.values()) for d in series.values()) * 1.05
    tmax = max(max(d.keys()) for d in series.values())
    s = [f"<svg xmlns='http://www.w3.org/2000/svg' width='{w}' height='{h}' viewBox='0 0 {w} {h}' {FONT}>",
         f"<rect width='{w}' height='{h}' fill='{SURFACE}'/>",
         f"<text x='24' y='34' font-size='19' font-weight='600' fill='{INK}'>Суммарный CPU во времени (прогон 1), % одного ядра</text>",
         f"<text x='24' y='54' font-size='13' fill='{INK2}'>Серая зона: запуск и прогрев ({warmup} с), в среднее не входит</text>"]
    for i, sc in enumerate(order):
        ox, oy = 40 + (i % cols) * (pw + gap), 92 + (i // cols) * (ph + 62)
        X = lambda t: ox + pw * t / tmax
        Y = lambda v: oy + ph - ph * v / vmax
        s.append(f"<text x='{ox}' y='{oy - 10}' font-size='14' font-weight='600' fill='{INK}'>{esc(labels[sc])}</text>")
        s.append(f"<rect x='{ox}' y='{oy}' width='{X(warmup) - ox:.1f}' height='{ph}' fill='#efeeea'/>")
        for v in range(0, int(vmax) + 1, nice_step(vmax)):
            s.append(f"<line x1='{ox}' x2='{ox + pw}' y1='{Y(v):.1f}' y2='{Y(v):.1f}' stroke='{GRID}'/>")
            s.append(f"<text x='{ox - 6}' y='{Y(v) + 4:.1f}' font-size='11' fill='{INK2}' text-anchor='end'>{v}</text>")
        pts = sorted(series[sc].items())
        s.append(f"<polyline fill='none' stroke='#2a78d6' stroke-width='2' stroke-linejoin='round' points='"
                 + " ".join(f"{X(t):.1f},{Y(v):.1f}" for t, v in pts) + "'/>")
        for t in range(0, int(tmax) + 1, 10):
            s.append(f"<text x='{X(t):.1f}' y='{oy + ph + 16}' font-size='11' fill='{INK2}' text-anchor='middle'>{t} с</text>")
    s.append("</svg>")
    path.write_text("\n".join(s))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--in", dest="src", default=str(ROOT / "perf" / "results"))
    src = Path(ap.parse_args().src)
    rows, meta = load(src)
    labels = {k: v for k, v in meta["scenarios"].items()}
    ru = {"Idle, Home": "Простой, Главная", "Scrolling Home (art shelves)": "Прокрутка Главной",
          "Scrolling Library → Songs": "Прокрутка Медиатеки → Песни", "Playback": "Воспроизведение",
          "Playback + synced lyrics": "Воспроизведение + текст", "Playback + EQ with analyzer": "Воспр. + эквалайзер с анализатором"}
    labels = {k: ru.get(v, v) for k, v in labels.items()}
    order = list(labels)
    cpu = aggregate(rows, "cpu_pct")
    mem = aggregate(rows, "pss_kb")
    gpu = aggregate(rows, "gpu_pct")
    cm = stacked_bars(cpu, labels, order, "Нагрузка на CPU по сценариям", "% одного ядра (среднее; усы = разброс между прогонами)", src / "cpu.svg")
    mm = stacked_bars(mem, labels, order, "Память (PSS) по сценариям", "МБ (PSS: общие страницы поделены честно между процессами)", src / "memory.svg")
    gm = stacked_bars(gpu, labels, order, "Нагрузка на GPU по сценариям", "% времени GPU (drm-engine из /proc/<pid>/fdinfo)", src / "gpu.svg", fmt="{:.1f}")
    timeline(rows, labels, order, meta["warmup_s"], src / "timeline.svg")
    if shutil.which("rsvg-convert"):
        for n in ("cpu", "memory", "gpu", "timeline"):
            subprocess.run(["rsvg-convert", "-z", "2", "-o", str(src / f"{n}.png"), str(src / f"{n}.svg")], check=True)

    eq = meta.get("eq_ticks_draws_per_s", {})
    lines = ["| Сценарий | CPU всего, % ядра | из них окно | движок | композитор | PSS, МБ | GPU, % |",
             "|---|---:|---:|---:|---:|---:|---:|"]
    for sc in order:
        c, m, g = cm[sc], mm[sc], gm[sc]
        eng = c["renderer"] + c["gpu"] + c["rest"]
        lines.append(f"| {labels[sc]} | {sum(c.values()):.1f} | {c['ui']:.1f} | {eng:.1f} | {c['compositor']:.1f} | "
                     f"{sum(v for k, v in m.items() if k != 'compositor'):.0f} | {sum(g.values()):.1f} |")
    if eq:
        lines.append("")
        for sc, runs in eq.items():
            t = sum(r[0] for r in runs) / len(runs)
            d = sum(r[1] for r in runs) / len(runs)
            lines.append(f"Эквалайзер ({labels.get(sc, sc)}): {t:.0f} тиков/с, {d:.0f} перерисовок графика/с.")
    (src / "table.md").write_text("\n".join(lines) + "\n")
    print("\n".join(lines))


if __name__ == "__main__":
    main()
