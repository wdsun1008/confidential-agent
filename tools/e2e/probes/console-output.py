#!/usr/bin/env python3
"""Trace actual guest console writes with an independent positive control."""
import json
import secrets
import subprocess
import time
from pathlib import Path


def command(*args):
    return subprocess.check_output(args, text=True)


def main():
    base = Path("/sys/kernel/tracing")
    mounted = not (base / "available_tracers").exists()
    if mounted:
        subprocess.run(["mount", "-t", "tracefs", "tracefs", str(base)], check=True)
    assert "function" in (base / "available_tracers").read_text().split()
    available = {line.split()[0] for line in (base / "available_filter_functions").read_text().splitlines() if line.split()}
    assert "redirected_tty_write" in available, "console write tracing is unavailable"
    nonce = secrets.token_hex(12)
    stdout_marker = "CA_APPLICATION_STDOUT_" + nonce
    stderr_marker = "CA_APPLICATION_STDERR_" + nonce
    canary = "CA_CONSOLE_" + nonce
    trace = base / "instances" / ("cai-console-" + nonce)
    script = Path("/run/cai-console-" + nonce + ".py")
    pidfile = Path("/run/cai-console-" + nonce + ".pid")
    dropin = Path("/run/systemd/system/cai-allinone.service.d/cai-console-" + nonce + ".conf")
    trace.mkdir()
    script.write_text(
        "from pathlib import Path\nimport os,sys\n"
        f"Path({str(pidfile)!r}).write_text(str(os.getpid()))\n"
        "if sys.argv[1] == 'journal':\n"
        f" print({stdout_marker!r}, flush=True)\n"
        f" print({stderr_marker!r}, file=sys.stderr, flush=True)\n"
        "else:\n"
        f" with open('/dev/console', 'w') as f: f.write({(canary + chr(10))!r})\n"
    )
    dropin.parent.mkdir(parents=True, exist_ok=True)

    def restart(mode):
        dropin.write_text(f"[Service]\nExecStartPre=/usr/bin/python3.11 {script} {mode}\n")
        command("systemctl", "daemon-reload")
        command("systemctl", "restart", "cai-allinone.service")

    def wrote_console(pid, text):
        return any(line.split() and line.split()[0].endswith(f"-{pid}") for line in text.splitlines())

    try:
        # /dev/console uses redirected_tty_write, unlike an ordinary TTY.
        filters = "*tty_write*" + (" devkmsg_write" if "devkmsg_write" in available else "")
        (trace / "set_ftrace_filter").write_text(filters)
        (trace / "current_tracer").write_text("function")
        (trace / "tracing_on").write_text("1")
        restart("journal")
        time.sleep(1)
        app_pid = int(pidfile.read_text())
        journal_pid = int(command("systemctl", "show", "systemd-journald", "-p", "MainPID", "--value"))
        assert journal_pid > 0
        journal = command("journalctl", "--no-pager")
        assert stdout_marker in journal and stderr_marker in journal, "application output missing from private journal"
        captured = (trace / "trace").read_text()
        assert not wrote_console(app_pid, captured), "application output reached a console or kernel log"
        assert not wrote_console(journal_pid, captured), "journal forwarded application output to a console or kernel log"
        (trace / "trace").write_text("")
        restart("console")
        time.sleep(1)
        assert wrote_console(int(pidfile.read_text()), (trace / "trace").read_text()), "positive console control was not traced"
        print(json.dumps({"passed": True, "stdout_marker": stdout_marker,
                          "stderr_marker": stderr_marker, "console_canary": canary}))
    finally:
        (trace / "tracing_on").write_text("0")
        (trace / "current_tracer").write_text("nop")
        trace.rmdir()
        dropin.unlink(missing_ok=True)
        script.unlink(missing_ok=True)
        pidfile.unlink(missing_ok=True)
        command("systemctl", "daemon-reload")
        command("systemctl", "restart", "cai-allinone.service")
        if mounted:
            command("umount", str(base))


if __name__ == "__main__":
    main()
