#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""治理红线核对：对实机会话 jsonl 执行与 session_replay.rs 完全同语义的 R1–R4 / A1–A2 度量。

用法：
    python -X utf8 governance_redline_check.py <会话jsonl|.harness/sessions目录> [--baseline <对照jsonl>]

会话日志位置：被操作项目的 <project>/.harness/sessions/<uuid>.jsonl（传目录自动取最近一条）。
退出码：0 全部红线通过；2 存在违例；1 用法/读取错误。非交互脚本，可直接跑。
"""
import argparse
import json
import re
import sys
from pathlib import Path

PROMPT_CAP = 300_000
A1_CAP = 12
A2_CAP = 2
CONTINUATION_CN = ("继续", "接着", "续", "恢复")
FAIL_OUTCOMES = {"Interrupted", "SystemFailure", "NeedsUserInput"}
BREAKER_OUTCOMES = {"Interrupted", "SystemFailure"}
SRC_EXTS = (".rs", ".toml", ".md", ".json", ".py", ".ts", ".slint")
WS_RE = re.compile(r"\s+")


def load_turns(path: Path):
    """折叠会话事件为逐回合摘要；不变量：日志恒以 TurnStart 开回合，此前事件丢弃。"""
    turns = []
    with path.open(encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            ev = json.loads(line)
            if "TurnStart" in ev:
                turns.append({
                    "input": ev["TurnStart"].get("input", ""),
                    "texts": [], "sigs": [], "prompt": 0, "outcome": None,
                })
            elif not turns:
                continue
            elif "Assistant" in ev:
                t = ev["Assistant"].get("chunk", {}).get("text")
                if t:
                    turns[-1]["texts"].append(t)
            elif "ToolCall" in ev:
                c = ev["ToolCall"].get("call", {})
                args = c.get("args")
                compact = json.dumps(args, separators=(",", ":"), ensure_ascii=False)
                turns[-1]["sigs"].append(f"{c.get('name','')}:{compact}")
            elif "Delivery" in ev:
                # 同回合多次 Delivery 取最后一条（运行时最终态）
                turns[-1]["outcome"] = (
                    ev["Delivery"].get("report", {}).get("outcome"))
            elif "Usage" in ev:
                turns[-1]["prompt"] += ev["Usage"].get("usage", {}).get("prompt_tokens", 0)
    return turns


def is_continuation(text: str) -> bool:
    t = text.strip()
    return t.startswith(CONTINUATION_CN) or t.lower().startswith(("continue", "resume"))


def has_path_anchor(text: str) -> bool:
    return any(
        ("/" in tok or "\\" in tok) and any(e in tok for e in SRC_EXTS)
        for tok in text.split()
    )


def is_exploratory(sig: str) -> bool:
    """A2 意图分类（与 session_replay.rs 同源，2026-09-01 实机判读）：
    回读/编译验证类重复是健康的交付自查不计；search 与其余 shell 属探索保留计入。"""
    if sig.startswith("search:") or sig.startswith("delegate:"):
        return True
    if sig.startswith("fs:"):
        return '"op":"edit"' in sig or '"op":"write"' in sig
    if sig.startswith("shell:"):
        return not any(k in sig for k in ("check", "build", "compile", "test", "py_compile"))
    if sig.startswith("plan:") or sig.startswith("memory:"):
        return False
    return True  # 未分类按探索计，宁严勿漏


def meters(turns):
    v = {}
    full = ["".join(t["texts"]) for t in turns]
    v["R1"] = [
        f"turn {i+1} input={t['input'][:30]!r}"
        for i, t in enumerate(turns)
        if is_continuation(t["input"]) and t["outcome"] == "NeedsUserInput"
    ]
    seen, r2 = {}, []
    for i, t in enumerate(turns):
        if t["outcome"] != "NeedsUserInput":
            continue
        key = WS_RE.sub("", full[i])
        if not key:
            continue
        if key in seen:
            r2.append(f"turn {seen[key]} 与 turn {i+1} 澄清文案完全相同")
        else:
            seen[key] = i + 1
    v["R2"] = r2
    v["R3"] = [] if sum(t["prompt"] for t in turns) <= PROMPT_CAP else [
        f"prompt 总量 {sum(t['prompt'] for t in turns)} > {PROMPT_CAP}"]
    v["R4"] = [
        f"turn {i+1} outcome={t['outcome']} 无锚点资产"
        for i, t in enumerate(turns)
        if t["outcome"] in FAIL_OUTCOMES and not has_path_anchor(full[i])
    ]
    a1 = sum(1 for t in turns if t["outcome"] in BREAKER_OUTCOMES) + \
        sum(1 for txt in full if "[需要澄清]" in txt)
    v["A1"] = [] if a1 <= A1_CAP else [f"守卫/熔断触发 {a1} > {A1_CAP}"]
    by_sig = {}
    for i, t in enumerate(turns):
        for sig in set(t["sigs"]):
            if is_exploratory(sig):
                by_sig.setdefault(sig, set()).add(i)
    a2 = max((len(s) for s in by_sig.values()), default=0)
    v["A2"] = [] if a2 <= A2_CAP else [f"同一工具签名跨 {a2} 个回合重复 > {A2_CAP}"]
    return v, a1, a2


def report(path: Path):
    turns = load_turns(path)
    print(f"\n=== {path.name}（{len(turns)} 回合）===")
    if not turns:
        print("  (无 TurnStart 事件)")
        return 1
    for i, t in enumerate(turns):
        print(f"  turn {i+1:>2} outcome={str(t['outcome']):<15} "
              f"prompt={t['prompt']:>7} tool_calls={len(t['sigs']):>2} input={t['input'][:28]!r}")
    v, a1, a2 = meters(turns)
    total_prompt = sum(t["prompt"] for t in turns)
    print(f"  合计 prompt={total_prompt}，A1 触发={a1}，A2 最大跨轮重复={a2}")
    bad = 0
    for name in ("R1", "R2", "R3", "R4", "A1", "A2"):
        if v[name]:
            bad += 1
            print(f"  [违例] {name}:")
            for item in v[name]:
                print(f"      - {item}")
        else:
            print(f"  [通过] {name}")
    return 1 if bad else 0


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("source", help="会话 jsonl 文件或 .harness/sessions 目录（取最近）")
    ap.add_argument("--baseline", help="对照会话 jsonl（如 7ba3370f 基线），同表并跑")
    args = ap.parse_args()

    def resolve(s):
        p = Path(s)
        if p.is_dir():
            cands = sorted(p.glob("*.jsonl"), key=lambda x: x.stat().st_mtime)
            if not cands:
                sys.exit(f"目录中无 jsonl: {p}")
            p = cands[-1]
        if not p.is_file():
            sys.exit(f"文件不存在: {p}")
        return p

    rc = report(resolve(args.source))
    if args.baseline:
        report(resolve(args.baseline))
        print("\n判定口径：实机新会话若比基线减少红线类别即为改善；全部 6 项通过才算本场景接管验收。")
    sys.exit(0 if rc == 0 else 2)


if __name__ == "__main__":
    main()
