import math, os, socket, sys, tempfile, time

# External animation can send frames using socket or pipe through stdin.

# python anim.py                      # direct frame socket
# python anim.py | zenvision stream   # pipe mode

W, H, FPS = 256, 64, 60

def main():
    if sys.stdout.isatty():
        # Run directly: own the frame socket and add the frame tag.
        runtime = os.environ.get("XDG_RUNTIME_DIR") or f"{tempfile.gettempdir()}/zenvision-{os.geteuid()}"
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        sock.connect(f"{runtime}/zenvision-frames.sock")
        def send(frame):
            sock.send(b"\0" + frame)
    else:
        # Piped: emit untagged frames to `zenvision stream`.
        def send(frame):
            sys.stdout.buffer.write(frame)
            sys.stdout.buffer.flush()

    start = deadline = time.monotonic()
    # Animate vertical line moving side to side across the display.
    while True:
        t = time.monotonic() - start
        x = int((W - 1) * (0.5 + 0.5 * math.sin(t * 2)))
        frame = bytearray(W * H)
        frame[x::W] = b"\xff" * H
        send(frame)
        deadline += 1 / FPS
        time.sleep(max(0, deadline - time.monotonic()))

try:
    main()
except (BrokenPipeError, ConnectionResetError, KeyboardInterrupt):
    os._exit(0)
