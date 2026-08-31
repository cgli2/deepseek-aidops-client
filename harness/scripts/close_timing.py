"""复现：启动 aidops-desktop.exe，向主窗口发 WM_CLOSE（等价点右上角 X），计时到进程退出。"""
import ctypes, subprocess, sys, time

user32 = ctypes.windll.user32
WM_CLOSE = 0x0010

exe = r"f:\workspace\deepseek-aidops-stable\harness\dist\aidops-desktop.exe"
env = {"HARNESS_WORKSPACE": r"f:\workspace\deepseek-aidops-stable\harness\dist"}
import os
full_env = dict(os.environ)
full_env.update(env)

print("launching...")
t0 = time.time()
p = subprocess.Popen([exe], env=full_env)

# 等待窗口出现（标题含 AIOPS Desktop）
hwnd = 0
def find_window(timeout=20.0):
    global hwnd
    deadline = time.time() + timeout
    buf = ctypes.create_unicode_buffer(512)
    while time.time() < deadline:
        found = 0
        def cb(h, _):
            global hwnd
            if user32.GetWindowTextW(h, buf, 512) and "AIOPS" in buf.value:
                hwnd = h
                return False
            return True
        EnumWindowsProc = ctypes.WINFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p)
        user32.EnumWindows(EnumWindowsProc(cb), 0)
        if found or hwnd:
            return True
        time.sleep(0.2)
    return False

if not find_window():
    print("window not found in 20s"); p.kill(); sys.exit(1)
print(f"window ready (hwnd={hwnd})，启动耗时 {time.time()-t0:.1f}s，等待 2s 稳定...")
time.sleep(2)

print("sending WM_CLOSE ...")
t1 = time.time()
user32.PostMessageW(hwnd, WM_CLOSE, 0, 0)
while p.poll() is None:
    if time.time() - t1 > 30:
        print("!! 30s 仍未退出，kill"); p.kill(); sys.exit(2)
    time.sleep(0.05)
elapsed = time.time() - t1
print(f"关闭耗时: {elapsed:.2f}s  (exit code = {p.returncode})")
