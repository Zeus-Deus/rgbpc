"""Private PTY lifecycle regression; OpenRGB is the inert CLI fixture."""
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import termios
import time
import tomllib

binary, root = sys.argv[1], Path(sys.argv[2])
(root / '.config/omarchy').mkdir(parents=True)
theme = root / '.local/state/omarchy/current/theme/colors.toml'
theme.parent.mkdir(parents=True)
theme.write_text('rgb = "#1234ab"\n')
master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 120, 0, 0))

def terminal_session():
    os.setsid()
    fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

process = subprocess.Popen([binary], stdin=slave, stdout=slave, stderr=slave,
                           preexec_fn=terminal_session)
os.close(slave)
transcript = bytearray()

def drain():
    while select.select([master], [], [], 0)[0]:
        try:
            part = os.read(master, 65536)
        except OSError:
            break
        if not part:
            break
        transcript.extend(part)

def visible(start=0):
    return re.sub(rb'\s', b'', re.sub(rb'\x1b\[[0-?]*[ -/]*[@-~]', b'', bytes(transcript[start:])))

def until(predicate, timeout=10):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        drain()
        if predicate():
            return
        if process.poll() is not None:
            raise AssertionError('TUI exited before completing the operation')
        time.sleep(.01)
    raise AssertionError('TUI operation timed out: ' + visible()[-1500:].decode(errors='replace'))

def commands():
    path = root / 'raw-commands.log'
    return path.read_text().splitlines() if path.exists() else []

def server_starts():
    return sum(line.startswith('--server ') for line in commands())

try:
    until(lambda: b'HardwareDevices' in visible())
    assert server_starts() == 1
    time.sleep(.2)
    drain()
    quiet_at = len(transcript)
    time.sleep(.6)
    drain()
    assert len(transcript) == quiet_at, f'idle TUI emitted {len(transcript)-quiet_at} unnecessary redraw bytes'
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack('HHHH', 42, 124, 0, 0))
    until(lambda: len(transcript) > quiet_at)
    os.write(master, b'R')
    until(lambda: sum('--list-devices' in line for line in commands()) >= 2)
    time.sleep(.2)  # The finite rescan worker must have returned.
    pid = int((root / 'server.pid').read_text())
    state = Path(f'/proc/{pid}/stat').read_text().split(') ', 1)[1].split()[0]
    assert state != 'Z', 'server died with its spawning rescan worker'
    assert server_starts() == 2
    os.write(master, b'c')
    until(lambda: b'PickColor' in visible())
    os.write(master, b'll\r')
    until(lambda: b'Colorcommandsaccepted' in visible())
    assert server_starts() == 2, 'warm color update unnecessarily restarted the rescan server'
    sync_started = len(transcript)
    os.write(master, b't')
    # Differential redraw may retain the existing "Sync" prefix on screen.
    until(lambda: b'commandsaccepted' in visible(sync_started))
    config_path = root / 'config/rgbpc/config.toml'
    def config():
        try:
            return tomllib.loads(config_path.read_text())
        except tomllib.TOMLDecodeError:
            return {}
    key = 'msi test board::motherboard'
    assert config()['saved_device_states'][key]['hex'] == '1234ab'
    os.write(master, b'a')
    until(lambda: config().get('restore_on_startup') is False)
    assert config()['saved_device_states'][key]['hex'] == '1234ab', 'settings toggle overwrote the completed theme sync with stale TUI state'
    os.write(master, b'q')
    process.wait(timeout=5)
    assert process.returncode == 0
    assert not Path(f'/proc/{pid}').exists(), 'owned rescan server was not reaped'
    print(json.dumps({'rescan_servers': 2, 'warm_server_reused': True, 'child_reaped': True}))
finally:
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait()
    drain()
    os.close(master)
