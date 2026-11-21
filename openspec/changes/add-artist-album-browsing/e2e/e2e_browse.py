"""Integration check for add-artist-album-browsing, task 7.1.

Drives the release build of kasset in a pty (answering DSR), renders its
output with pyte, and asserts each step. Usage:

    python3 -m venv venv && venv/bin/pip install pyte
    cargo build --release
    venv/bin/python e2e_browse.py target/release/kasset

Needs Linux (pty, ALSA output). Builds its library and state folder in a
temporary folder (kept, and its path printed, if a check fails), and writes
the recorded screens and the final state file to e2e_browse_screens.txt
next to this script.
"""

import fcntl
import math
import os
import pty
import re
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
import wave

import pyte

ROWS, COLS = 40, 120
RESULTS = []
SCREENS = []
RUNNING = []  # every Kasset started, killed on the way out if still alive


def check(name, ok, detail=""):
    RESULTS.append((name, ok))
    print(f"{'PASS' if ok else 'FAIL'}: {name}" + (f"  [{detail}]" if detail else ""))


def tone(path, freq, seconds, rate=22050):
    frames = bytearray()
    for i in range(int(rate * seconds)):
        frames += struct.pack("<h", int(8000 * math.sin(2 * math.pi * freq * i / rate)))
    with wave.open(path, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(bytes(frames))


def id3_frame(fid, text):
    data = b"\x03" + text.encode("utf-8")  # ID3v2.4, UTF-8
    size = len(data)
    syncsafe = bytes([(size >> 21) & 0x7F, (size >> 14) & 0x7F, (size >> 7) & 0x7F, size & 0x7F])
    return fid.encode() + syncsafe + b"\x00\x00" + data


def add_id3(path, frames):
    """Append an `id3 ` chunk holding an ID3v2.4 tag to a WAV file."""
    body = b"".join(id3_frame(f, t) for f, t in frames)
    size = len(body)
    header = b"ID3\x04\x00\x00" + bytes(
        [(size >> 21) & 0x7F, (size >> 14) & 0x7F, (size >> 7) & 0x7F, size & 0x7F]
    )
    tag = header + body
    chunk = b"id3 " + struct.pack("<I", len(tag)) + tag + (b"\x00" if len(tag) % 2 else b"")
    with open(path, "r+b") as f:
        data = f.read()
        assert data[:4] == b"RIFF"
        data = data + chunk
        data = data[:4] + struct.pack("<I", len(data) - 8) + data[8:]
        f.seek(0)
        f.write(data)
        f.truncate()


def make_library(root):
    """Alpha (First, Second), Beta (Gamma), and a tagged compilation."""
    plain = [
        ("Alpha", "First", ["One", "Two"]),
        ("Alpha", "Second", ["Three"]),
        ("Beta", "Gamma", ["Four", "Five"]),
    ]
    freq = 220
    for artist, album, titles in plain:
        folder = os.path.join(root, artist, album)
        os.makedirs(folder, exist_ok=True)
        for n, title in enumerate(titles, 1):
            tone(os.path.join(folder, f"0{n} - {title}.wav"), freq, 60)
            freq += 55
    # Compilation: album order Zed, Aaron, Moe; library order (by track
    # artist) puts Aaron first and Alpha after him.
    folder = os.path.join(root, "Compilations", "Mixtape")
    os.makedirs(folder, exist_ok=True)
    for n, (artist, title, secs) in enumerate(
        [("Zed", "Opener", 4), ("Aaron", "Middle", 60), ("Moe", "Closer", 60)], 1
    ):
        path = os.path.join(folder, f"0{n}.wav")
        tone(path, freq, secs)
        freq += 55
        add_id3(path, [("TIT2", title), ("TPE1", artist), ("TALB", "Mixtape"),
                       ("TPE2", "Various Artists"), ("TRCK", str(n))])


class Kasset:
    def __init__(self, binary, library, state_home):
        self.screen = pyte.Screen(COLS, ROWS)
        self.stream = pyte.ByteStream(self.screen)
        self.lock = threading.Lock()
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        env = dict(os.environ, XDG_STATE_HOME=state_home, TERM="xterm-256color")
        self.proc = subprocess.Popen(
            [binary, library], stdin=slave, stdout=slave, stderr=subprocess.PIPE,
            env=env, start_new_session=True,
        )
        os.close(slave)
        self.master = master
        self.alive = True
        RUNNING.append(self)
        self.reader = threading.Thread(target=self._pump, daemon=True)
        self.reader.start()
        self.wait_for(lambda s: "vol " in s, 10, "first frame")
        time.sleep(0.5)  # the first key right after startup can be lost

    def _pump(self):
        while self.alive:
            try:
                r, _, _ = select.select([self.master], [], [], 0.1)
                if not r:
                    continue
                data = os.read(self.master, 65536)
            except OSError:
                break
            if not data:
                break
            if b"\x1b[6n" in data:
                os.write(self.master, b"\x1b[1;1R")
            with self.lock:
                self.stream.feed(data)

    def text(self):
        with self.lock:
            return "\n".join(self.screen.display)

    def record(self, label):
        SCREENS.append((label, self.text()))

    def wait_for(self, pred, timeout, what):
        end = time.time() + timeout
        while time.time() < end:
            if pred(self.text()):
                return True
            time.sleep(0.05)
        raise RuntimeError(f"timeout waiting for {what}:\n{self.text()}")

    def send(self, keys, pause=0.2):
        for k in keys:
            os.write(self.master, k.encode())
            time.sleep(pause)

    def track_line(self):
        for line in self.text().splitlines():
            if "vol " in line:
                return line
        return ""

    def now(self):
        m = re.search(r"([▶‖■]) (\w+) — (\w+) · (\w+)", self.track_line())
        return m.groups() if m else None

    def main_title(self):
        # The main pane's top border, right of the sidebar.
        boxes = re.findall(r"┌ ([^┐]*?) ─*┐", self.text().splitlines()[0])
        return boxes[1].strip() if len(boxes) > 1 else ""

    def finish(self, timeout=10):
        if not self.alive:
            return self.proc.returncode, ""
        try:
            code = self.proc.wait(timeout)
        finally:
            self.alive = False
            self.reader.join()
            os.close(self.master)
        err = self.proc.stderr.read().decode(errors="replace")
        lines = err.splitlines(keepends=True)
        if lines and lines[0].startswith("Scanning ") and lines[0].rstrip().endswith("…"):
            lines = lines[1:]
        return code, "".join(lines)

    def quit(self):
        self.send("q")
        return self.finish()


def body_lines(text):
    """The lines of the pane row (between the top and bottom borders)."""
    out = []
    for line in text.splitlines()[1:]:
        if line.startswith("└"):
            break
        out.append(line)
    return out


def main_cell(text, name):
    """The main pane's cell of the body line whose row starts with `name`
    (after the marker column), or "" if there is none."""
    for line in body_lines(text):
        parts = line.split("│")
        if len(parts) > 3 and re.match(rf"^ [▶‖■ ] {re.escape(name)}\b", parts[3]):
            return parts[3]
    return ""


def sidebar_row(text, name):
    """The sidebar's part of the line whose sidebar row is `name`."""
    for line in text.splitlines():
        parts = line.split("│")
        if len(parts) > 2 and re.match(rf"^ [▶‖■ ] {name}\b", parts[1]):
            return parts[1]
    return ""


def selected(k, pane):
    """Text of the focused selection (reverse video) in pane 0 (sidebar) or
    1 (main), or None. An unfocused pane's selection is not reverse video,
    so this also tells which pane has focus."""
    with k.lock:
        for y in range(1, ROWS):
            line = k.screen.display[y]
            if line.startswith("└"):
                break
            bars = [x for x, c in enumerate(line) if c == "│"]
            if len(bars) < 2 * pane + 2:
                continue
            lo, hi = bars[2 * pane] + 1, bars[2 * pane + 1]
            if any(k.screen.buffer[y][x].reverse for x in range(lo, hi)):
                return line[lo:hi].strip()
    return None


def main():
    binary = os.path.abspath(sys.argv[1])
    work = tempfile.mkdtemp(prefix="kasset-e2e-browse-")
    library = os.path.join(work, "Music")
    state_home = os.path.join(work, "state")
    os.makedirs(state_home)
    make_library(library)
    state_file = os.path.join(state_home, "kasset", "state")
    ok = False
    try:
        ok = run(binary, library, state_home, state_file)
    finally:
        for p in RUNNING:
            if p.proc.poll() is None:
                p.proc.kill()
                p.finish()
        if ok:
            shutil.rmtree(work)
        else:
            print(f"kept work folder: {work}")
    sys.exit(0 if ok else 1)


def run(binary, library, state_home, state_file):
    k = Kasset(binary, library, state_home)
    k.record("start")
    # Sidebar: Library, Artists, Albums.
    k.send("\t")
    k.wait_for(lambda s: "Artists" in s, 3, "sidebar")
    k.send("j\r")
    k.wait_for(lambda s: "Various Artists" in s, 3, "artist list")
    k.record("artist list")
    s = k.text()
    check("artist list shows only album artists",
          all(a in s for a in ("Alpha", "Beta", "Various Artists"))
          and not any(re.search(rf"│\s*{a}\b", s) for a in ("Zed", "Aaron", "Moe")),
          k.main_title())
    check("Alpha has 2 albums and 3 tracks", re.search(r"Alpha\s+2\s+3", s) is not None, main_cell(s, "Alpha"))
    check("Various Artists has 1 album and 3 tracks", re.search(r"Various Artists\s+1\s+3", s) is not None,
          main_cell(s, "Various Artists"))
    check("bar says Esc sidebar", "Esc sidebar" in s)

    # Alpha: All tracks, First, Second.
    k.send("\r")  # first row is Alpha
    k.wait_for(lambda s: "All tracks" in s, 3, "Alpha's albums")
    k.record("Alpha's albums")
    s = k.text()
    lines = s.splitlines()
    order = [next((i for i, l in enumerate(lines) if n in l), -1) for n in ("All tracks", "First", "Second")]
    check("Alpha's albums: All tracks, First, Second", -1 not in order and order == sorted(order), str(order))
    check("bar says Esc back", "Esc back" in s)
    k.send("\x7f")  # Backspace pops to the artist list
    k.wait_for(lambda s: "Various Artists" in s and "All tracks" not in s, 3, "back to artists")
    check("Backspace returns to the artist list", "Beta" in k.text())

    # Various Artists → Mixtape.
    k.send("G\r")
    k.wait_for(lambda s: "Mixtape" in s and "All tracks" in s, 3, "VA albums")
    k.send("G\r")
    k.wait_for(lambda s: "Opener" in s and "Closer" in s, 3, "Mixtape tracks")
    k.record("Mixtape tracks")
    s = k.text()
    lines = s.splitlines()
    order = [next((i for i, l in enumerate(lines) if n in l), -1) for n in ("Opener", "Middle", "Closer")]
    check("Mixtape in album order", -1 not in order and order == sorted(order), str(order))
    check("pane titled with album and artist", "Mixtape · Various Artists" in lines[0], lines[0])
    check("bar says Enter play and Esc back", "Enter play" in s and "Esc back" in s)

    # Play the first track (4 s) and let it advance within the album.
    k.send("g\r")
    k.wait_for(lambda s: k.now() and k.now()[1] == "Opener", 3, "Opener playing")
    check("Opener plays", k.now()[0] == "▶", str(k.now()))
    k.wait_for(lambda s: k.now() and k.now()[1] == "Middle", 10, "advance to Middle")
    k.record("advanced to Middle")
    check("advances to Middle (album order, not library order)", k.now()[:3] == ("▶", "Middle", "Aaron"),
          str(k.now()))
    s = k.text()
    check("Middle's row is marked", main_cell(s, "Middle").startswith(" ▶"), main_cell(s, "Middle"))

    # Back out: Backspace → VA albums (Mixtape marked), Esc → artists (VA
    # marked), Esc → sidebar with Artists selected and "Albums" marked.
    k.send("\x7f")
    k.wait_for(lambda s: "All tracks" in s and "Opener" not in s, 3, "back to VA albums")
    k.record("back to VA albums")
    check("Mixtape row is marked", main_cell(k.text(), "Mixtape").startswith(" ▶"), main_cell(k.text(), "Mixtape"))
    k.send("\x1b")
    k.wait_for(lambda s: "Alpha" in s and "All tracks" not in s, 3, "back to artists")
    k.record("back to artists")
    check("Various Artists row is marked", main_cell(k.text(), "Various Artists").startswith(" ▶"),
          main_cell(k.text(), "Various Artists"))
    check("Various Artists row is selected", selected(k, 1) == "▶ Various Artists                     1      3",
          str(selected(k, 1)))
    k.send("\x1b")
    time.sleep(0.4)
    k.record("back to sidebar")
    s = k.text()
    check("Esc at the top focuses the sidebar (sidebar bar shown)", "Esc sidebar" not in s and "Enter open" in s,
          s.splitlines()[-1])
    sel = selected(k, 0)
    check("sidebar has focus with Artists selected", sel is not None and re.match(r"Artists\s+3$", sel) is not None,
          str(sel))
    check("sidebar marks Albums as the play order", "▶" in sidebar_row(s, "Albums"), sidebar_row(s, "Albums"))
    # Back to the same place: Enter on Artists shows the list with Various
    # Artists still selected.
    k.send("\r")
    k.wait_for(lambda s: selected(k, 1) is not None, 3, "artist list focused")
    k.record("artists again")
    check("Enter on Artists returns to the same selection",
          (selected(k, 1) or "").startswith("▶ Various Artists") and selected(k, 0) is None, str(selected(k, 1)))

    # Let some time pass in Middle, then quit.
    time.sleep(3)
    at_quit = k.now()
    code, err = k.quit()
    check("quit exits 0 with empty stderr", code == 0 and err == "", f"{code} {err!r}")
    data = open(state_file, "rb").read()
    print("--- state file ---")
    print(data.decode(errors="replace"), end="")
    print("------------------")
    check("state file saves the album source", b"source album Various%20Artists Mixtape\n" in data)
    check("state file saves the track path", b"track Compilations/Mixtape/02.wav\n" in data)
    check("state file saves item 1", re.search(rb"\nitem 1\n", data) is not None)
    pos = re.search(rb"position-ms (\d+)", data)
    check("state file has a position of at least 2 s", pos is not None and int(pos.group(1)) >= 2000,
          pos.group(1).decode() if pos else "none")

    # Restart: Middle resumes paused; next follows the album (Closer), not
    # the library (Alpha's One).
    k = Kasset(binary, library, state_home)
    k.record("restart")
    check("restart resumes Middle paused", k.now() is not None and k.now()[:2] == ("‖", "Middle"), str(k.now()))
    check("library shown after restart", "Library" in k.main_title(), k.main_title())
    check("sidebar marks Albums after restart", "‖" in sidebar_row(k.text(), "Albums") or "▶" in sidebar_row(k.text(), "Albums"),
          sidebar_row(k.text(), "Albums"))
    k.send("n")
    k.wait_for(lambda s: k.now() and k.now()[1] != "Middle", 3, "next track")
    k.record("next after restart")
    check("next follows the album: Closer by Moe", k.now()[1:3] == ("Closer", "Moe"), str(k.now()))
    code, err = k.quit()
    check("second quit exits 0", code == 0 and err == "", f"{code} {err!r}")

    out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "e2e_browse_screens.txt")
    with open(out, "w") as f:
        for label, text in SCREENS:
            f.write(f"===== {label} =====\n{text}\n")
        f.write("===== state file =====\n" + data.decode(errors="replace"))
    print(f"screens: {out}")
    failed = [n for n, ok in RESULTS if not ok]
    print(f"\n{len(RESULTS) - len(failed)}/{len(RESULTS)} passed")
    return not failed


if __name__ == "__main__":
    main()
