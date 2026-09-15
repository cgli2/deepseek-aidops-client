#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""工作区全面检查/验证脚本（非交互，可直接跑）。

做两件事：
  1) 按 harness/Cargo.toml 的 members，逐个 crate 用 MSVC 工具链做
     `cargo check`（lib + bin + tests 类型检查），汇总通过/失败；
  2) 对 .harness/sessions 最新一条会话跑治理红线核对（复用
     governance_redline_check.py 的同源度量）。

用法：
    python -X utf8 harness/scripts/verify_workspace.py            # 全量
    python -X utf8 harness/scripts/verify_workspace.py --quick    # 只检查改动 crate
退出码：0 全部通过；1 存在编译失败或治理违例。
"""
import argparse
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "harness" / "Cargo.toml"
TOOLCHAIN = "+stable-x86_64-pc-windows-msvc"
REDLINE = ROOT / "harness" / "scripts" / "governance_redline_check.py"
SESSIONS = ROOT / ".harness" / "sessions"
QUICK_CRATES = ["harness-runtime"]  # 近期改动涉及的 crate


def run(cmd):
    p = subprocess.run(cmd, capture_output=True, text=True,
                       encoding="utf-8", errors="replace")
    return p.returncode, p.stdout, p.stderr


def workspace_members():
    text = MANIFEST.read_text(encoding="utf-8")
    block = re.search(r"members\s*=\s*\[(.*?)\]", text, re.S)
    if not block:
        raise SystemExit("无法解析 harness/Cargo.toml 的 members")
    return re.findall(r'"([^"]+)"', block.group(1))


def package_name(member):
    """成员目录 -> 实际 package 名（bin 目录的包名是 harness-bin）。"""
    toml = ROOT / "harness" / member / "Cargo.toml"
    m = re.search(r'name\s*=\s*"([^"]+)"', toml.read_text(encoding="utf-8"))
    return m.group(1) if m else member


def check_crate(member):
    pkg = package_name(member)
    cmd = ["cargo", TOOLCHAIN, "check", "--manifest-path", str(MANIFEST),
           "-p", pkg, "--all-targets"]
    rc, out, err = run(cmd)
    log = err + out
    errs = len(re.findall(r"^error", log, re.M))
    warns = len(re.findall(r"^warning:", log, re.M))
    status = "通过" if rc == 0 else "失败"
    print(f"  [{status}] {member:26s} ({pkg}) rc={rc} errors={errs} warnings={warns}")
    if rc != 0:
        print("\n".join(log.strip().splitlines()[-25:]))
    return rc == 0


def latest_session():
    if not SESSIONS.is_dir():
        return None
    files = sorted(SESSIONS.glob("*.jsonl"),
                   key=lambda p: p.stat().st_mtime, reverse=True)
    return files[0] if files else None


def check_governance():
    s = latest_session()
    if not s:
        print("  未找到会话日志，跳过治理核对")
        return True
    rc, out, err = run([sys.executable, "-X", "utf8", str(REDLINE), str(s)])
    print(f"  会话 {s.name} -> rc={rc}")
    print((out or err).strip())
    return rc == 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--quick", action="store_true", help="只检查改动相关 crate")
    args = ap.parse_args()

    crates = QUICK_CRATES if args.quick else workspace_members()
    print(f"=== 1) 编译检查（{len(crates)} 个 crate，MSVC 工具链）===")
    compile_ok = all([check_crate(c) for c in crates]) and True

    print("\n=== 2) 治理红线核对 ===")
    gov_ok = check_governance()

    print("\n=== 汇总 ===")
    print(f"编译检查: {'全部通过' if compile_ok else '存在失败'}")
    print(f"治理核对: {'通过' if gov_ok else '存在违例/错误'}")
    sys.exit(0 if (compile_ok and gov_ok) else 1)


if __name__ == "__main__":
    main()
