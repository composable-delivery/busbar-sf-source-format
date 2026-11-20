#!/usr/bin/env python3
"""
Apples-to-apples performance benchmark runner comparing:
  Part A: In-process engines (Rust `sf-source-format` vs Node `@salesforce/source-deploy-retrieve@13.13.0`)
  Part B: Cold CLI execution (Rust `sf-source-format` vs Salesforce CLI `sf`)

Outputs a human-readable Markdown summary to stdout and machine-readable JSON to
`benchmarks/results.json`.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
BENCH_DIR = REPO_ROOT / "benchmarks"
NODE_BENCH_DIR = BENCH_DIR / "node-sdr"
RESULTS_PATH = Path(os.environ.get("BENCH_RESULTS_PATH", BENCH_DIR / "results.json"))
EXE = ".exe" if os.name == "nt" else ""

RUST_CLI_BIN = REPO_ROOT / "target" / "release" / f"sf-source-format{EXE}"
RUST_HARNESS_BIN = REPO_ROOT / "target" / "release" / "examples" / f"bench_harness{EXE}"
NODE_HARNESS_SCRIPT = NODE_BENCH_DIR / "bench.mjs"
GENERATOR_SCRIPT = BENCH_DIR / "generate-workloads.mjs"

MAX_RSS_RE = re.compile(r"^\s*(\d+)\s+maximum resident set size", re.MULTILINE)
PEAK_FOOTPRINT_RE = re.compile(r"^\s*(\d+)\s+peak memory footprint", re.MULTILINE)
GNU_MAX_RSS_RE = re.compile(r"Maximum resident set size \(kbytes\):\s*(\d+)")


def run_cmd(
    cmd: list[str],
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
    measure_time: bool = False,
) -> tuple[subprocess.CompletedProcess[str], float, int | None, int | None]:
    time_bin = Path("/usr/bin/time")
    system = platform.system()
    time_mode: str | None = None
    if measure_time and time_bin.exists():
        if system == "Darwin":
            full_cmd = [str(time_bin), "-l", *cmd]
            time_mode = "darwin"
        elif system == "Linux":
            full_cmd = [str(time_bin), "-v", *cmd]
            time_mode = "linux"
        else:
            full_cmd = cmd
    else:
        full_cmd = cmd
    started = time.perf_counter()
    proc = subprocess.run(
        full_cmd,
        cwd=str(cwd or REPO_ROOT),
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )
    elapsed_ms = (time.perf_counter() - started) * 1000.0
    if proc.returncode != 0:
        raise RuntimeError(
            f"Command failed ({proc.returncode}): {' '.join(full_cmd)}\n"
            f"--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}"
        )
    max_rss_bytes = None
    peak_footprint_bytes = None
    if time_mode == "darwin":
        m = MAX_RSS_RE.search(proc.stderr)
        if m:
            max_rss_bytes = int(m.group(1))
        fp = PEAK_FOOTPRINT_RE.search(proc.stderr)
        if fp:
            peak_footprint_bytes = int(fp.group(1))
    elif time_mode == "linux":
        m = GNU_MAX_RSS_RE.search(proc.stderr)
        if m:
            max_rss_bytes = int(m.group(1)) * 1024
    return proc, elapsed_ms, max_rss_bytes, peak_footprint_bytes


def summarize(samples_ms: list[float]) -> dict[str, float]:
    sorted_vals = sorted(samples_ms)
    n = len(sorted_vals)
    p95_idx = min(n - 1, math.ceil(0.95 * n) - 1)
    return {
        "min_ms": min(sorted_vals),
        "median_ms": statistics.median(sorted_vals),
        "mean_ms": statistics.fmean(sorted_vals),
        "p95_ms": sorted_vals[p95_idx],
        "max_ms": max(sorted_vals),
        "samples_ms": samples_ms,
    }


def collect_env_info() -> dict[str, str]:
    info: dict[str, str] = {}
    commands = {
        "rustc": ["rustc", "--version"],
        "node": ["node", "--version"],
        "sf_cli": ["sf", "--version"],
        "os": ["sw_vers"],
        "cpu": ["sysctl", "-n", "machdep.cpu.brand_string"],
        "cores": ["sysctl", "-n", "hw.ncpu"],
        "ram_bytes": ["sysctl", "-n", "hw.memsize"],
    }
    for key, cmd in commands.items():
        try:
            proc = subprocess.run(cmd, text=True, capture_output=True, check=False)
            if proc.returncode == 0:
                lines = [ln.strip() for ln in proc.stdout.splitlines() if ln.strip() and not ln.strip().startswith("›")]
                info[key] = " | ".join(lines)
        except OSError:
            info[key] = "unavailable"
    pkg_json = NODE_BENCH_DIR / "node_modules" / "@salesforce" / "source-deploy-retrieve" / "package.json"
    if pkg_json.exists():
        info["sdr_version"] = json.loads(pkg_json.read_text())["version"]
    return info


def build_artifacts() -> None:
    print("[setup] Building release Rust binaries...", flush=True)
    run_cmd(
        [
            "cargo", "build", "--release", "-p", "busbar-sf-source-format",
            "--bin", "sf-source-format", "--example", "bench_harness",
        ]
    )

    if not (NODE_BENCH_DIR / "node_modules").exists():
        print("[setup] Installing Node @salesforce/source-deploy-retrieve@13.13.0...", flush=True)
        run_cmd(["npm", "install", "--no-audit", "--no-fund"], cwd=NODE_BENCH_DIR)


def count_files(dir_path: Path) -> int:
    total = 0
    for root, _, files in os.walk(dir_path):
        del root
        total += len(files)
    return total


def build_in_process_workloads(generated: dict) -> list[dict]:
    fixtures = generated["fixtures"]
    synthetic = generated["synthetic"]
    workloads: list[dict] = []

    def add_pair(entry: dict, warmup: int, iters: int, *, include_zip: bool, mdapi_to_src: bool = True) -> None:
        name = entry["name"]
        project_dir = entry["projectDir"]
        out_root = Path(project_dir) / "_bench_out"
        if mdapi_to_src and entry["mdapiDir"]:
            w_name = f"{name} (MDAPI -> SFDX)"
            workloads.append(
                {
                    "name": w_name,
                    "direction": "mdapi_to_source",
                    "mode": "directory",
                    "input_dir": entry["mdapiDir"],
                    "project_dir": project_dir,
                    "output_base_dir": str(out_root / f"out_{len(workloads)}"),
                    "warmup": warmup,
                    "iterations": iters,
                }
            )
        workloads.append(
            {
                "name": f"{name} (SFDX -> MDAPI dir)",
                "direction": "source_to_mdapi",
                "mode": "directory",
                "input_dir": entry["sfdxDir"],
                "project_dir": project_dir,
                "output_base_dir": str(out_root / f"out_{len(workloads)}"),
                "warmup": warmup,
                "iterations": iters,
            }
        )
        if include_zip:
            workloads.append(
                {
                    "name": f"{name} (SFDX -> MDAPI zip)",
                    "direction": "source_to_mdapi",
                    "mode": "zip",
                    "input_dir": entry["sfdxDir"],
                    "project_dir": project_dir,
                    "output_base_dir": str(out_root / f"out_{len(workloads)}"),
                    "warmup": warmup,
                    "iterations": iters,
                }
            )

    for fx_name, inc_zip in [
        ("customObjects-and-children", True),
        ("digitalExperienceBundle", True),
        ("staticResource", False),
        ("preset-decomposedPS2", False),
        ("preset-decomposedESR", False),
    ]:
        add_pair(fixtures[fx_name], 3, 10, include_zip=inc_zip)

    add_pair(fixtures["customLabels-multiple"], 3, 10, include_zip=False, mdapi_to_src=False)
    add_pair(synthetic["medium-org"], 2, 5, include_zip=True)
    add_pair(synthetic["large-org"], 1, 3, include_zip=True)
    real_world = generated.get("realWorld", {})
    if "npsp" in real_world:
        add_pair(real_world["npsp"], 1, 3, include_zip=True)
    return workloads


def run_in_process_comparison(generated: dict, scratch: Path) -> dict:
    bench_tmp = scratch / "in_process"
    bench_tmp.mkdir(parents=True, exist_ok=True)
    workloads = build_in_process_workloads(generated)

    rust_workloads = json.loads(json.dumps(workloads))
    for w in rust_workloads:
        w["output_base_dir"] = str(Path(w["output_base_dir"]) / "rust")
    rust_manifest = bench_tmp / "rust_manifest.json"
    rust_manifest.write_text(json.dumps({"workloads": rust_workloads}, indent=2))

    node_workloads = json.loads(json.dumps(workloads))
    for w in node_workloads:
        w["output_base_dir"] = str(Path(w["output_base_dir"]) / "node")
    node_manifest = bench_tmp / "node_manifest.json"
    node_manifest.write_text(json.dumps({"workloads": node_workloads}, indent=2))

    print(f"[part A] Running Rust release harness ({len(workloads)} workloads)...", flush=True)
    rust_proc, _, rust_rss, rust_fp = run_cmd(
        [str(RUST_HARNESS_BIN), str(rust_manifest)], measure_time=True
    )
    rust_json = json.loads(rust_proc.stdout)

    print(f"[part A] Running Node SDR 13.13.0 harness ({len(workloads)} workloads)...", flush=True)
    node_proc, _, node_rss, node_fp = run_cmd(
        ["node", str(NODE_HARNESS_SCRIPT), str(node_manifest)], measure_time=True
    )
    node_json = json.loads(node_proc.stdout)

    rust_by_name = {item["name"]: item for item in rust_json["results"]}
    node_by_name = {item["name"]: item for item in node_json["results"]}

    compared = []
    for w in workloads:
        name = w["name"]
        r = rust_by_name[name]
        n = node_by_name[name]
        r_stats = summarize(r["durations_ms"])
        n_stats = summarize(n["durations_ms"])
        count_match = r["output_count"] == n["output_count"]
        if not count_match:
            print(
                f"  [WARN] output_count mismatch for '{name}': rust={r['output_count']} vs node={n['output_count']}",
                file=sys.stderr,
            )
        compared.append(
            {
                "name": name,
                "direction": w["direction"],
                "mode": w["mode"],
                "rust": {**r_stats, "output_count": r["output_count"]},
                "node": {**n_stats, "output_count": n["output_count"]},
                "speedup_median": n_stats["median_ms"] / r_stats["median_ms"],
                "speedup_mean": n_stats["mean_ms"] / r_stats["mean_ms"],
                "output_count_match": count_match,
            }
        )

    return {
        "workloads": compared,
        "process_memory": {
            "rust_max_rss_bytes": rust_rss,
            "rust_peak_footprint_bytes": rust_fp,
            "node_max_rss_bytes": node_rss,
            "node_peak_footprint_bytes": node_fp,
        },
    }


def run_cold_cli_comparison(generated: dict, scratch: Path) -> list[dict]:
    del scratch
    sf_env = os.environ.copy()
    sf_env.update(
        {
            "SF_DISABLE_AUTOUPDATE": "true",
            "SF_SKIP_NEW_VERSION_CHECK": "true",
            "SF_DISABLE_TELEMETRY": "true",
            "SF_USE_PROGRESS_BAR": "false",
            "SF_NO_COLOR": "true",
        }
    )

    targets = [
        (generated["fixtures"]["customObjects-and-children"], 5),
        (generated["fixtures"]["digitalExperienceBundle"], 5),
        (generated["synthetic"]["medium-org"], 5),
    ]
    real_world = generated.get("realWorld", {})
    if "npsp" in real_world:
        targets.append((real_world["npsp"], 3))

    results = []
    warmup = 1

    for entry, iterations in targets:
        for direction in ("mdapi_to_source", "source_to_mdapi"):
            label = f"{entry['name']} ({'MDAPI -> SFDX' if direction == 'mdapi_to_source' else 'SFDX -> MDAPI'})"
            proj_dir = Path(entry["projectDir"])
            print(f"[part B] Cold CLI: {label}...", flush=True)

            engine_stats: dict[str, dict] = {}
            for engine in ("rust_cli", "sf_cli"):
                samples_ms: list[float] = []
                rss_samples: list[int] = []
                last_count = 0

                for idx in range(warmup + iterations):
                    out_dir = proj_dir / "_bench_cli_out" / f"{direction}_{engine}_{idx}"
                    shutil.rmtree(out_dir, ignore_errors=True)
                    out_dir.mkdir(parents=True, exist_ok=True)

                    if engine == "rust_cli":
                        if direction == "mdapi_to_source":
                            cmd = [
                                str(RUST_CLI_BIN),
                                "convert",
                                "source",
                                "--sourcepath",
                                entry["mdapiDir"],
                                "--output-dir",
                                str(out_dir),
                                "--project-dir",
                                str(proj_dir),
                                "--exact-dir",
                            ]
                        else:
                            cmd = [
                                str(RUST_CLI_BIN),
                                "convert",
                                "mdapi",
                                "--sourcepath",
                                entry["sfdxDir"],
                                "--output-dir",
                                str(out_dir),
                                "--project-dir",
                                str(proj_dir),
                                "--exact-dir",
                            ]
                    else:
                        if direction == "mdapi_to_source":
                            cmd = [
                                "sf",
                                "project",
                                "convert",
                                "mdapi",
                                "-r",
                                entry["mdapiDir"],
                                "-d",
                                str(out_dir),
                            ]
                        else:
                            cmd = [
                                "sf",
                                "project",
                                "convert",
                                "source",
                                "-p",
                                entry["sfdxDir"],
                                "-d",
                                str(out_dir),
                            ]

                    _, wall_ms, max_rss, _ = run_cmd(
                        cmd, cwd=proj_dir, env=sf_env, measure_time=True
                    )
                    if idx >= warmup:
                        samples_ms.append(wall_ms)
                        if max_rss is not None:
                            rss_samples.append(max_rss)
                    last_count = count_files(out_dir)
                    shutil.rmtree(out_dir, ignore_errors=True)

                stats = summarize(samples_ms)
                stats["max_rss_bytes"] = max(rss_samples) if rss_samples else None
                stats["output_count"] = last_count
                engine_stats[engine] = stats

            count_match = (
                engine_stats["rust_cli"]["output_count"]
                == engine_stats["sf_cli"]["output_count"]
            )
            if not count_match:
                print(
                    f"  [WARN] CLI output_count mismatch for '{label}': "
                    f"rust={engine_stats['rust_cli']['output_count']} vs sf={engine_stats['sf_cli']['output_count']}",
                    file=sys.stderr,
                )

            results.append(
                {
                    "name": label,
                    "direction": direction,
                    "rust_cli": engine_stats["rust_cli"],
                    "sf_cli": engine_stats["sf_cli"],
                    "speedup_median": engine_stats["sf_cli"]["median_ms"]
                    / engine_stats["rust_cli"]["median_ms"],
                    "speedup_mean": engine_stats["sf_cli"]["mean_ms"]
                    / engine_stats["rust_cli"]["mean_ms"],
                    "output_count_match": count_match,
                }
            )

    return results


def print_markdown_summary(data: dict) -> None:
    print("\n=== Part A: In-Process Engine Comparison (Rust vs Node SDR 13.13.0) ===")
    print("| Workload | Rust Median (ms) | Node Median (ms) | Speedup | Output Files (R / N) |")
    print("| --- | ---: | ---: | ---: | :---: |")
    for row in data["part_a"]["workloads"]:
        r_med = row["rust"]["median_ms"]
        n_med = row["node"]["median_ms"]
        sp = row["speedup_median"]
        rc = row["rust"]["output_count"]
        nc = row["node"]["output_count"]
        print(f"| {row['name']} | {r_med:.2f} | {n_med:.2f} | {sp:.2f}x | {rc} / {nc} |")

    mem = data["part_a"]["process_memory"]
    if mem["rust_max_rss_bytes"] and mem["node_max_rss_bytes"]:
        r_mb = mem["rust_max_rss_bytes"] / (1024 * 1024)
        n_mb = mem["node_max_rss_bytes"] / (1024 * 1024)
        print(f"\nFull-suite Peak RSS: Rust = {r_mb:.1f} MiB, Node SDR = {n_mb:.1f} MiB ({n_mb / r_mb:.2f}x lower)")

    print("\n=== Part B: Cold CLI Comparison (`sf-source-format` vs `sf`) ===")
    if not data["part_b"]:
        print("_Skipped (`--skip-sf-cli` or `sf` CLI unavailable)._")
        return
    print("| Workload | Rust CLI Median (ms) | `sf` CLI Median (ms) | Speedup | Rust Peak RSS | `sf` Peak RSS |")
    print("| --- | ---: | ---: | ---: | ---: | ---: |")
    for row in data["part_b"]:
        r_med = row["rust_cli"]["median_ms"]
        s_med = row["sf_cli"]["median_ms"]
        sp = row["speedup_median"]
        r_rss = (row["rust_cli"]["max_rss_bytes"] or 0) / (1024 * 1024)
        s_rss = (row["sf_cli"]["max_rss_bytes"] or 0) / (1024 * 1024)
        print(f"| {row['name']} | {r_med:.1f} | {s_med:.1f} | {sp:.2f}x | {r_rss:.1f} MiB | {s_rss:.1f} MiB |")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Run Rust vs Node SDR / sf CLI benchmarks.")
    parser.add_argument(
        "--skip-sf-cli",
        action="store_true",
        help="Skip Part B cold CLI comparison against the Salesforce `sf` CLI.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    build_artifacts()
    env_info = collect_env_info()

    with tempfile.TemporaryDirectory(prefix="sf-source-format-bench-") as tmp_str:
        scratch = Path(tmp_str)
        projects_root = scratch / "projects"
        print("[setup] Preparing fixtures & synthetic enterprise orgs...", flush=True)
        gen_proc, _, _, _ = run_cmd(
            ["node", str(GENERATOR_SCRIPT), str(projects_root), str(RUST_CLI_BIN)]
        )
        generated = json.loads(gen_proc.stdout)

        part_a = run_in_process_comparison(generated, scratch)
        has_sf_cli = shutil.which("sf") is not None
        if args.skip_sf_cli or not has_sf_cli:
            if not args.skip_sf_cli and not has_sf_cli:
                print("[part B] Skipping cold CLI comparison (`sf` CLI not found on PATH).", flush=True)
            part_b = []
        else:
            part_b = run_cold_cli_comparison(generated, scratch)

    payload = {
        "environment": env_info,
        "part_a": part_a,
        "part_b": part_b,
    }
    RESULTS_PATH.write_text(json.dumps(payload, indent=2) + "\n")
    print_markdown_summary(payload)
    print(f"\nWrote machine-readable results to {RESULTS_PATH}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
