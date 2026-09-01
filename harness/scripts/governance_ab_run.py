#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""实机三场景 A/B 对照编排器（阶段 2 Task 10 自动化）。

用 stdio JSON-RPC 驱动 `aidops-desktop.exe --acp` 跑真实模型多回合会话，
逐场景收集 <workspace>/.harness/sessions/*.jsonl，按 governance_redline_check
的同源度量打分并汇总 summary.md。非交互，直接运行。

用法：
    python -X utf8 scripts/governance_ab_run.py --smoke                # 单回合链路冒烟
    python -X utf8 scripts/governance_ab_run.py                        # S1-S3（控制器）+ 基线对照
    python -X utf8 scripts/governance_ab_run.py --modes controller,legacy

退出码：0 控制器场景全绿；2 存在违例；1 工具/链路错误。
"""
import argparse
import json
import os
import queue
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import governance_redline_check as gr  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
EXE = REPO / "harness" / "dist" / "aidops-desktop.exe"
BASELINE = REPO / "harness" / "harness-runtime" / "tests" / "fixtures" / "7ba3370f_full.jsonl"

SYMPTOM = ("点击项目文件树按钮的时候，或者git diff 按钮的时候，界面就会不断出现一个黑cmd闪烁的"
           "窗口，虽然只是一瞬间，但是请修复。")
GITFIX = ("git diff 报错，请修复：\n\n无法读取 Git 状态：git error: git rev-parse "
          "--show-toplevel failed: not a git repository。请检查并恢复 git 集成。")

SCENARIOS = {
    "S1": {"turns": [SYMPTOM, "继续完成任务", "继续"], "git": True},
    "S2": {"turns": ["这个问题解决了吗？", "继续", "继续", "继续，你自己解决呀"], "git": False},
    "S3": {"turns": [GITFIX, "修复完成了吗？为什么任务会中断？", "继续"], "git": True},
}

# compose.rs 里 settings 的 workspace.root 优先于 HARNESS_WORKSPACE：
# 不临时改写就会全部跑进用户真实项目。另持久化的 llm.model 可能是端点无路由的
# 历史值（kimi-k3 在 ark 上 404），回退 selected_models 首选。首次改写前记原值，退出统一还原。
_settings_backup = {"done": False, "rows": {}}
PROFILE = ""  # --profile 指定的 model_profiles 名；置空则沿用当前 llm.* 设置


def _settings_db() -> Path:
    portable = EXE.parent / "DeepSeekAIOps" / "settings.db"
    if portable.is_file():
        return portable
    return Path(os.environ.get("LOCALAPPDATA", "")) / "DeepSeekAIOps" / "settings.db"


def _settings_override(pairs, db_path):
    """pairs: dict[key, str|bytes]；bytes 视为密文原样写 is_secret=1（如 api_key）。
    一次连接内备份并改写，atexit 统一还原。"""
    import atexit
    import sqlite3
    if not db_path.is_file():
        return
    conn = sqlite3.connect(str(db_path))
    if not _settings_backup["done"]:
        _settings_backup["done"] = True
        atexit.register(_restore_settings, conn)
    for key, value in pairs.items():
        if key not in _settings_backup["rows"]:
            _settings_backup["rows"][key] = conn.execute(
                "SELECT value,is_secret FROM settings WHERE key=?", (key,)).fetchone()
        col, sec = (value.encode("utf-8"), 0) if isinstance(value, str) else (bytes(value), 1)
        conn.execute(
            "INSERT INTO settings(key,value,is_secret,updated_at) "
            "VALUES(?,?,?,CURRENT_TIMESTAMP) "
            "ON CONFLICT(key) DO UPDATE SET value=excluded.value,is_secret=excluded.is_secret,updated_at=CURRENT_TIMESTAMP",
            (key, col, sec))  # 必须 BLOB：Rust 侧 Vec<u8> 拒绝 TEXT
    conn.commit()


def apply_profile(name: str):
    """用 model_profiles 某行覆盖 llm.*（切换端点/密钥/模型），原值同样进备份。"""
    import sqlite3
    db = _settings_db()
    conn = sqlite3.connect(str(db))
    row = conn.execute(
        "SELECT provider,base_url,model,api_key FROM model_profiles WHERE name=?",
        (name,)).fetchone()
    conn.close()
    if not row or not row[3]:
        sys.exit(f"model_profiles 无此配置或密钥缺失: {name}")
    provider, base_url, model, key = row
    dec = lambda v: v.decode() if isinstance(v, (bytes, bytearray)) else str(v)
    _settings_override({"llm.provider": dec(provider), "llm.base_url": dec(base_url),
                        "llm.model": dec(model), "llm.api_key": bytes(key)}, db)


def _restore_settings(conn):
    for key, row in _settings_backup["rows"].items():
        if row is None:
            conn.execute("DELETE FROM settings WHERE key=?", (key,))
        else:
            conn.execute("UPDATE settings SET value=?,is_secret=? WHERE key=?",
                         (row[0], row[1], key))
    conn.commit()
    conn.close()


def override_settings(work: Path):
    pairs = {"workspace.root": str(work)}
    db = _settings_db()
    if db.is_file() and not PROFILE:
        import sqlite3
        conn = sqlite3.connect(str(db))
        cur_model = conn.execute(
            "SELECT value FROM settings WHERE key='llm.model'").fetchone()
        selected = conn.execute(
            "SELECT value FROM settings WHERE key='llm.selected_models'").fetchone()
        conn.close()
        sel = (selected[0].decode() if selected and isinstance(selected[0], bytes)
               else (selected[0] if selected else "")) or ""
        cur = (cur_model[0].decode() if cur_model and isinstance(cur_model[0], bytes)
               else (cur_model[0] if cur_model else "")) or ""
        pool = [m for m in sel.split(",") if m.strip()]
        if cur not in pool and pool:
            pairs["llm.model"] = pool[0].strip()
    _settings_override(pairs, db)


class AcpClient:
    """行分隔 JSON-RPC 客户端；后台线程读 stdout（响应）与 stderr（落日志）。"""

    def __init__(self, proc, stderr_path: Path):
        self.proc = proc
        self.responses = queue.Queue()
        self.id = 0
        threading.Thread(target=self._pump, daemon=True).start()
        threading.Thread(target=self._drain_stderr, args=(stderr_path,), daemon=True).start()

    def _pump(self):
        for line in self.proc.stdout:
            line = line.strip()
            if line:
                self.responses.put(line)

    def _drain_stderr(self, path: Path):
        with path.open("w", encoding="utf-8", errors="replace") as f:
            for line in self.proc.stderr:
                f.write(line)
                f.flush()

    def call(self, method, params=None, timeout=900.0):
        self.id += 1
        msg = {"jsonrpc": "2.0", "id": self.id, "method": method}
        if params:
            msg["params"] = params
        self.proc.stdin.write(json.dumps(msg, ensure_ascii=False) + "\n")
        self.proc.stdin.flush()
        deadline = time.monotonic() + timeout
        while True:
            remain = max(0.1, deadline - time.monotonic())
            try:
                raw = self.responses.get(timeout=remain)
            except queue.Empty:
                raise TimeoutError(f"{method} 超时 {timeout}s")
            try:
                resp = json.loads(raw)
            except json.JSONDecodeError:
                continue  # 非协议行（理论不该出现）忽略
            if resp.get("id") == self.id:
                if resp.get("error"):
                    raise RuntimeError(f"{method} 报错: {resp['error']}")
                return resp.get("result")

    def close(self, timeout=60):
        try:
            self.proc.stdin.close()  # server 读到 EOF 自然退出
            self.proc.wait(timeout=timeout)
        except Exception:
            self.proc.kill()


def seed_workspace(work: Path, with_git: bool):
    work.mkdir(parents=True, exist_ok=True)
    (work / "app").mkdir(exist_ok=True)
    (work / "app" / "main.py").write_text(
        "def ui():\n    print('demo tree button')\n\nif __name__ == '__main__':\n    ui()\n",
        encoding="utf-8")
    (work / "README.md").write_text("# demo project\n\n用于实机 A/B 对照的靶项目。\n", encoding="utf-8")
    if with_git:
        win_noconsole = {"creationflags": 0x08000000} if os.name == "nt" else {}
        for args in (["init", "-q"], ["add", "-A"],
                     ["-c", "user.email=ab@local", "-c", "user.name=ab", "commit", "-qm", "seed"]):
            subprocess.run(["git", "-C", str(work)] + args, check=True,
                           capture_output=True, **win_noconsole)
        (work / "app" / "main.py").write_text(
            "def ui():\n    print('demo tree button')  # 待修复：黑框闪烁\n", encoding="utf-8")


def run_scenario(tag, mode, turns, outdir: Path, turn_timeout=900.0):
    work = outdir / "work" / f"{tag}-{mode}"
    if work.exists():
        shutil.rmtree(work)
    seed_workspace(work, SCENARIOS.get(tag, {}).get("git", False))
    env = dict(os.environ)
    env["HARNESS_WORKSPACE"] = str(work)
    if mode == "controller":
        env["HARNESS_GOVERNOR"] = "on"
    else:
        env.pop("HARNESS_GOVERNOR", None)
    print(f"\n>>> {tag}/{mode}: {len(turns)} 回合，工作区 {work}")
    override_settings(work)
    if PROFILE:
        apply_profile(PROFILE)
    note = ""
    proc = subprocess.Popen(
        [str(EXE), "--acp"], cwd=work, env=env,
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        encoding="utf-8", errors="replace", bufsize=1)
    client = AcpClient(proc, outdir / f"{tag}-{mode}.stderr.log")
    try:
        client.call("initialize", timeout=300)
        print(f"    [{tag}] ACP 握手完成")
        for i, text in enumerate(turns):
            t0 = time.monotonic()
            client.call("session/prompt", {"text": text}, timeout=turn_timeout)
            print(f"    [{tag}] turn {i+1}/{len(turns)} 完成 "
                  f"({time.monotonic()-t0:.0f}s): {text[:24]!r}")
    except TimeoutError as e:
        note = f"回合超时挂死（{e}）——计为红线级失败"
        print(f"    [{tag}] !! {note}")
    except (RuntimeError, OSError, ValueError) as e:
        note = f"链路错误: {e}"
        print(f"    [{tag}] !! {note}")
    finally:
        if proc.poll() is None:
            client.close()
    sess_dir = work / ".harness" / "sessions"
    cands = sorted(sess_dir.glob("*.jsonl"), key=lambda p: p.stat().st_mtime) if sess_dir.is_dir() else []
    dest = outdir / f"{tag}-{mode}.jsonl"
    if cands:
        shutil.copyfile(cands[-1], dest)
    else:
        note = (note + "；" if note else "") + "未找到会话 jsonl（回合可能全部失败）"
    return dest if cands else None, note


METERS = ("R1", "R2", "R3", "R4", "A1", "A2")


def score(jsonl: Path):
    turns = gr.load_turns(jsonl)
    v, a1, a2 = gr.meters(turns)
    outcomes = [str(t["outcome"]) for t in turns]
    llm_error = any("llm provider error" in "".join(t["texts"]) for t in turns)
    clean = not any(v[m] for m in METERS)
    return turns, v, outcomes, clean, a1, a2, llm_error


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--smoke", action="store_true", help="单回合廉价输入验证全链路")
    ap.add_argument("--scenarios", default="S1,S2,S3")
    ap.add_argument("--modes", default="controller", help="controller / legacy / 逗号组合")
    ap.add_argument("--outdir", default=None)
    ap.add_argument("--turn-timeout", type=float, default=900.0)
    ap.add_argument("--profile", default="",
                    help="model_profiles 行名，切到可用端点（ark 欠费时用），如 'openai · deepseek-v4-pro'")
    args = ap.parse_args()
    global PROFILE
    PROFILE = args.profile

    if not EXE.is_file():
        sys.exit(f"缺少可执行文件: {EXE}（先跑 scripts/build.bat package）")
    stamp = time.strftime("%Y%m%d-%H%M%S", time.localtime())
    outdir = Path(args.outdir) if args.outdir else REPO / "harness" / "ab-runs" / stamp
    outdir.mkdir(parents=True, exist_ok=True)

    rows = []
    all_clean = True
    if args.smoke:
        plan = [("SMOKE", m, ["只回复两个字：收到。不要调用任何工具。"])
                for m in args.modes.split(",")]
        SCENARIOS["SMOKE"] = {"turns": plan[0][2], "git": False}
    else:
        plan = [(s, m, SCENARIOS[s]["turns"])
                for s in args.scenarios.split(",") for m in args.modes.split(",")]

    for tag, mode, turns_in in plan:
        jsonl, note = run_scenario(tag, mode, turns_in, outdir, args.turn_timeout)
        if jsonl is None:
            rows.append((tag, mode, 0, "-", "链路失败", note))
            all_clean = False
            continue
        turns, v, outcomes, clean, a1, a2, llm_err = score(jsonl)
        if llm_err:
            clean = False
        bad = [m for m in METERS if v[m]]
        all_clean &= clean
        detail = "; ".join(f"{m}: {v[m][0]}" for m in bad) + \
            ("；模型报错仍交付（假绿守卫拦截）" if llm_err else "") + \
            ("；" + note if note else "")
        rows.append((tag, mode, len(turns), ",".join(outcomes),
                     "全绿" if clean else ("链路失败" if llm_err and not bad
                                     else "违例 " + ",".join(bad)), detail[:300]))

    if not args.smoke and "legacy" not in args.modes.split(","):
        turns, v, outcomes, clean, a1, a2, _ = score(BASELINE)
        rows.append(("基线", "legacy(7ba3370f)", len(turns), "见 jsonl",
                     "全绿" if clean else "违例 " + ",".join(m for m in METERS if v[m]),
                     "历史真实失败模式，作为 Legacy 对照列"))

    md = ["# 实机 A/B 对照结果 " + stamp, "",
          "| 场景 | 模式 | 回合 | outcomes | 判定 | 明细 |", "|---|---|---|---|---|---|"]
    md += [f"| {r[0]} | {r[1]} | {r[2]} | {r[3]} | {r[4]} | {r[5]} |" for r in rows]
    (outdir / "summary.md").write_text("\n".join(md) + "\n", encoding="utf-8")
    print("\n" + "\n".join(md))
    print(f"\n产物目录: {outdir}")
    sys.exit(0 if all_clean else 2)


if __name__ == "__main__":
    main()
