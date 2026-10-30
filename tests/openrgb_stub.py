"""Inert CLI integration-test server: never imports or invokes OpenRGB."""
import os
import socket
import sys
import struct

mode = os.environ.get("RGBPC_TEST_SERVER_FAIL", "")
if mode == "exit":
    sys.exit(9)
if mode == "bind":
    print("Could not bind network socket", flush=True)
    # Keep the process alive to prove failed-start cleanup reaps it.
    import time
    time.sleep(60)
    sys.exit(9)
listener = socket.socket()
listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
listener.bind(("127.0.0.1", 6742))
listener.listen()
if mode == "bytes":
    sys.stdout.buffer.write(b"non-UTF-8 device name: \xff\n")
    sys.stdout.buffer.flush()
print("Detection completed", flush=True)
count = 0
while True:
    connection, _ = listener.accept()
    with connection:
        connection.settimeout(1)
        try:
            header = connection.recv(16, socket.MSG_WAITALL)
            if len(header) == 16 and header[:4] == b"ORGB":
                _, device, packet, size = struct.unpack("<4sIII", header)
                if size:
                    connection.recv(size, socket.MSG_WAITALL)
                if packet == 0 and mode != "mute":
                    connection.sendall(struct.pack("<4sIIII", b"ORGB", device, 0, 4, 1))
                    count += 1
                    if mode == "drop" or (mode == "drop-after-two" and count == 2):
                        sys.exit(0)
        except (OSError, TimeoutError):
            pass
