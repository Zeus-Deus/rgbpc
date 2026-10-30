"""Inert SDK v3 peer for color-finalization tests; never accesses hardware."""
import json
import os
from pathlib import Path
import socket
import struct
import time

STATE_PATH = Path(os.environ["RGBPC_TEST_SDK_STATE"])
BEHAVIOR = os.environ.get("RGBPC_TEST_SERVER_FAIL", "solid")
states = [{"active_mode": 0, "static_color_mode": 3, "brightness": 8,
           "colors": ["000000", "000000"], "mode_writes": 0, "color_writes": 0,
           "finishes": 0, "readbacks": [], "late_version_replies": 0} for _ in range(2 if BEHAVIOR == "solid-duplicates" else 1)]


def ignore_writes(device):
    return (BEHAVIOR == "solid-ignore"
            or (BEHAVIOR == "solid-duplicates" and device == 0)
            or (BEHAVIOR == "solid-second-ignore" and states[device]["finishes"] >= 2))


def save():
    temporary = STATE_PATH.with_suffix(".new")
    temporary.write_text(json.dumps(dict(states[0], devices=states)))
    temporary.replace(STATE_PATH)


def string(value):
    data = value.encode() + b"\0"
    return struct.pack("<H", len(data)) + data


def mode(name, value, flags, color_mode, brightness):
    return (string(name) + struct.pack("<12I", value, flags, 0, 0, 0, 10,
                                     0, 0, 0, brightness, 0, color_mode)
            + struct.pack("<H", 0))


def description(device):
    state = states[device]
    data = struct.pack("<I", 0)
    for value in ["MSI Test Board", "Test Vendor", "Synthetic controller", "", "", ""]:
        data += string(value)
    data += struct.pack("<HI", 2, state["active_mode"])
    data += mode("Direct", 100, 32, 1, state["brightness"])
    data += mode("Static", 1, 304, state["static_color_mode"], state["brightness"])
    data += struct.pack("<H", 0)  # No zones; two independently described LEDs.
    data += struct.pack("<H", len(state["colors"]))
    for index in range(len(state["colors"])):
        data += string(f"LED {index}") + struct.pack("<I", index)
    data += struct.pack("<H", len(state["colors"]))
    data += b"".join(bytes.fromhex(color) + b"\0" for color in state["colors"])
    return struct.pack("<I", len(data) + 4) + data


def receive_exact(connection, count):
    data = b""
    while len(data) < count:
        block = connection.recv(count - len(data))
        if not block:
            raise EOFError
        data += block
    return data


def reply(connection, device, packet, data):
    connection.sendall(struct.pack("<4sIII", b"ORGB", device, packet, len(data)) + data)


save()
listener = socket.socket()
listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
listener.bind(("127.0.0.1", 6742))
listener.listen()
print("Detection completed", flush=True)
while True:
    connection, _ = listener.accept()
    with connection:
        connection.settimeout(3)
        queried = False
        try:
            while True:
                header = receive_exact(connection, 16)
                magic, device, packet, size = struct.unpack("<4sIII", header)
                assert magic == b"ORGB" and size <= 1_048_576
                body = receive_exact(connection, size)
                if packet == 0:
                    reply(connection, device, packet, struct.pack("<I", len(states)))
                elif packet == 40:
                    assert body == struct.pack("<I", 3)
                    if BEHAVIOR == "solid-late-version" and states[0]["finishes"] >= 1:
                        states[0].update(active_mode=0, static_color_mode=3, colors=["000000"] * 2)
                        time.sleep(.4)
                        states[0]["late_version_replies"] += 1
                        save()
                    reply(connection, device, packet, struct.pack("<I", 6))
                elif packet == 1:
                    assert device < len(states) and body == struct.pack("<I", 3)
                    state = states[device]
                    if not queried:
                        state["finishes"] += 1
                        if BEHAVIOR == "solid-second-ignore" and state["finishes"] >= 2:
                            # The second CLI pass leaves a different state.
                            state.update(active_mode=0, static_color_mode=3, colors=["000000"] * 2)
                        queried = True
                        save()
                    else:
                        state["readbacks"].append({"active_mode": state["active_mode"], "colors": list(state["colors"])})
                        save()
                    reply_device = device + 1 if BEHAVIOR == "solid-wrong-device" else device
                    reply(connection, reply_device, packet, description(device))
                elif packet == 1101:
                    assert device < len(states) and len(body) == struct.unpack_from("<I", body)[0]
                    state = states[device]
                    index, name_size = struct.unpack_from("<IH", body, 4)
                    assert index == 1 and body[10:10 + name_size] == b"Static\0"
                    fields = struct.unpack_from("<12I", body, 10 + name_size)
                    assert fields == (1, 304, 0, 0, 0, 10, 0, 0, 0, 8, 0, 1)
                    assert struct.unpack_from("<H", body, 10 + name_size + 48)[0] == 0
                    state["mode_writes"] += 1
                    if not ignore_writes(device):
                        state["active_mode"] = index
                        state["static_color_mode"] = fields[11]
                    save()
                elif packet == 1050:
                    declared, count = struct.unpack_from("<IH", body)
                    assert device < len(states) and declared == len(body) == 6 + 4 * count
                    state = states[device]
                    assert count == len(state["colors"])
                    state["color_writes"] += 1
                    if not ignore_writes(device):
                        state["colors"] = [body[offset:offset + 3].hex()
                                           for offset in range(6, len(body), 4)]
                    save()
                else:
                    raise AssertionError(f"Unexpected inert SDK request {packet}")
        except (EOFError, OSError, TimeoutError):
            pass
